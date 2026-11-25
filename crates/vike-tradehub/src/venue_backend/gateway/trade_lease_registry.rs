//! `TradeLeaseRegistry` — where the control door gets the FACTS and the PRICE of a symbol its engine was not
//! started on (decisions 0173 and 0174): the node asks the gatewayhub over the account's OWN open
//! trade lease, because the node holds no venue connection and no key but the Trade key.
//!
//! The lease answers `TradeUp::SymbolProperties` from the venue's public instrument list
//! (`vike_gatewayhub_client::trade::TradeLease::symbol_facts`). This type is the node's side of it:
//!
//! * **One registry of open leases, by ROUTE KEY**, filled by `VenueBackendGateway::start_adapter` when the
//!   adapter answers `Live` and shared by every view of the connection (the plan loop's and the
//!   mount's backends are two views of one). An account with no entry has no lease: the answer is
//!   that, by name, never a guess from another account's lease.
//! * **A cache of answers**, keyed `(route key, symbol)`, valid for [`FACTS_TTL`] and only while the daemon's
//!   reconcile client was told every symbol this node admitted (`TradeLease::has_told`, forgotten at a gatewayhub restart): an order does not
//!   cost a round trip to the venue's instrument list. Only a SUCCESS is cached.
//! * **A refresh** ([`SymbolAdmitter::refresh`]) for a symbol this source admitted: once the answer is
//!   older than [`REFRESH_AFTER`] the next order on the symbol re-reads the facts and the door sends
//!   them as an ordinary `AdmitSymbol` refresh, so a tick or lot the venue changed is applied within
//!   minutes, not for the engine's life.
//! * **The two per-symbol money facts an answer may carry.** The symbol's own LEVERAGE (decision 0182's
//!   per-symbol twin) comes from the daemon's COMMIT only and is kept per `(route key, symbol)`
//!   ([`SymbolAdmitter::confirmed_leverage`]); a symbol quoted in ANOTHER currency than the account's (decision
//!   0183, phase 1: OANDA's crosses on a demo account) carries its quote currency and a conversion rate
//!   ([`super::quoted::judge`] refuses an answer without a usable, fresh one), which is reused for
//!   [`RATE_FRESH_FOR`] only, re-read by the next order that finds it older, and kept inside the engine's
//!   window for a held position by the timer ([`TradeLeaseRegistry::spawn_rate_refresher`]): one long-lived worker per
//!   venue, so one slow venue delays no other venue's tick, the restored positions the boot missed healed, and
//!   a held position whose rate nothing refreshes named at `warn` every minute with its age
//!   ([`TradeLeaseRegistry::unrefreshed_held`]).
//! * **A price for an admitted symbol** ([`SymbolAdmitter::price`]): the first committing ask subscribes
//!   the symbol's tick pump on its venue's feed dial ([`TradeLeaseRegistry::bind_prices`], bound once the
//!   feeds are wired), on the grid the admission carries; every ask reads the last price that pump
//!   delivered. A symbol the door gave up on before admitting is released ([`SymbolAdmitter::release`]):
//!   its pump stays open for [`IDLE_TTL`] (the operator's retry finds the same subscription), then
//!   [`TradeLeaseRegistry::close_idle_pumps`] closes it, and the number of OPEN pumps of symbols nobody
//!   admitted is bounded ([`MAX_WATCHED`]); a full set gives its oldest idle pump up before it
//!   refuses. The pump of an ADMITTED symbol is never closed here.
//! * **A held lane for the engine's OWN symbol** (decision 0197; a LABELLED engine's too since 0198): a
//!   `demo`/`live` engine that holds a position or a live order in the symbol it was started on has that
//!   symbol's price lane kept open ([`TradeLeaseRegistry::hold_price`], [`SymbolAdmitter::hold_lane`]), because nothing
//!   else prices it unless a strategy is mounted there. A held lane is the ENGINE's (like an admitted symbol's):
//!   it takes no place under [`MAX_WATCHED`], the reaper never closes it, and it goes back to the reaper when
//!   the engine is flat with no order in the symbol ([`TradeLeaseRegistry::keep_lanes`], the timer
//!   [`TradeLeaseRegistry::spawn_lane_keeper`]). Several accounts of one venue holding one symbol share ONE subscription
//!   (the dial dedupes by symbol, `held_on_venue` counts every route key of the venue), and a labelled engine's
//!   symbol is also MARKED (`sync_labelled_mark`): the core hands a venue's quote to the venue's default engine
//!   only.
//! * **A combo's LEGS are committed, not admitted** ([`TradeLeaseRegistry::tell_combo_legs`]): the core writes an accepted
//!   combo's legs into its engine itself and tells this registry (`vike_core::CoreConfig::on_combo_legs`,
//!   [`TradeLeaseRegistry::combo_leg_teller`]), and the boot tells it the legs the journal restored
//!   (`crate::readmit::combo_legs`). Each leg takes the admission's commit to the daemon alone, so its reconcile
//!   client reads the leg and a restarted gatewayhub is told it again (`TradeLease::retell_symbols`); no slot or
//!   price lane of this registry's. On the admitted-pair reaper's tick ([`TradeLeaseRegistry::settle_combo_legs`]) a commit
//!   the gatewayhub could not answer is asked again (bounded), and a leg no live combo and no position needs any more
//!   is forgotten at the daemon, which frees its read and its place there; the engine keeps listing it.
//! * **The boot window** ([`TradeLeaseRegistry::begin_boot`]): while the boot re-admission runs on the boot
//!   thread, every gatewayhub round trip waits at most the boot bound (`config.gatewayhub_connect_wait_ms`,
//!   the bound `VenueBackendGateway::start_adapter` uses) and a lease that missed it is not asked again until
//!   the window ends. The runtime any-symbol door keeps the lease's own `request_reply`.
//!
//! ⚠ No lock is held across a network call (a lease RPC, a datahub subscribe): a slow lookup must not
//! queue other orders, the 50 ms price polls or a Telegram `/panic` behind it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_execution::{Command, SeriesKey, SymbolAdmission, VenueBlock};
use vike_gatewayhub_client::credentialed_limits::CredentialedStreamLogin;
use vike_gatewayhub_client::trade::{SymbolPropertiesRequestError, TradeLease, TradeLinkState};
use vike_model::accounts::account_keys::{AccountLabel, venue_of_route_key};
use vike_model::{SymbolGrid, VenueRead, VenueReadKind};
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub_client::wire::{PriceLinkDownWire, TradeLinkDownWire, VenueLinkDownWire};
use vike_tradehub_core::ReconcileFirstBalance;
use vike_utility::errors::FailureOutcome;
use vike_venues::reconnect::ReconnectBackoff;

use crate::feeds::StrategyLaneCexWatcher;
use crate::feeds::price_links::PriceLinks;
use crate::server::symbol_admission::{
    AccountReadiness, BalanceWait, LinkHealth, SymbolAdmitter, VenueReads,
};

/// How long a looked-up admission is reused by [`SymbolAdmitter::admission`].
pub const FACTS_TTL: Duration = Duration::from_secs(3600);

/// How old an admitted symbol's facts may get before the next order re-reads them.
pub const REFRESH_AFTER: Duration = Duration::from_secs(600);

/// How long a failed refresh waits before the next order tries again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// **How long a conversion rate is reused** (decision 0183, phase 1): an answer that carries one is served,
/// and counts as fresh, for this long and no longer, where the facts around it live [`FACTS_TTL`] and are
/// refreshed every [`REFRESH_AFTER`]. The engine calls a rate stale at 120 s and then denies opening orders,
/// so an order older than this finds its rate re-read BEFORE it is judged (the door's refresh), and the
/// timer ([`TradeLeaseRegistry::spawn_rate_refresher`]) keeps a held position's rate inside the window with no order.
pub const RATE_FRESH_FOR: Duration = Duration::from_secs(30);

/// How long a failed rate refresh waits before the next order (or the next tick) tries again.
const RATE_RETRY_AFTER: Duration = Duration::from_secs(10);

/// How often the rate timer re-reads the rate of a held foreign-quoted position (decision 0183, phase 1:
/// about once a minute): two chances inside the engine's 120 s window.
pub const RATE_REFRESH_EVERY: Duration = Duration::from_secs(60);

/// The first wait before the rate timer tries a restored foreign-quoted position the boot missed AGAIN after a
/// failed try; each further failure doubles it, up to [`OWED_RETRY_CAP_FACTOR`] times this (15 minutes). The heal
/// asks the gateway's data lane, rationed by the hour (`crates/bridges/vike-ibkr/src/transport/gateway_gate.rs`'s
/// `DataDials`): tried every tick (a minute) by every owed symbol it would use that ration up and refuse every new
/// symbol until the hour rolled.
const OWED_RETRY_FIRST: Duration = Duration::from_secs(60);

/// The most [`OWED_RETRY_FIRST`] is multiplied by: the wait between two tries of one owed symbol never exceeds it.
const OWED_RETRY_CAP_FACTOR: u32 = 15;

/// How long the rate timer waits before it looks again at a boot window that is still open
/// ([`TradeLeaseRegistry::spawn_rate_refresher`]).
const BOOT_RECHECK: Duration = Duration::from_secs(1);

/// How long a definitive refusal is remembered by [`SymbolAdmitter::admission`].
const NEGATIVE_TTL: Duration = Duration::from_secs(30);

/// The most price pumps this process holds OPEN for symbols that were not mounted and are not (yet)
/// admitted: a place is held from the subscribe until the pump is closed, not until the door lets go
/// of it. Admitted symbols are capped at the engine (`vike_execution::MAX_EXTRA_SYMBOLS` per account), so a
/// stream of refused symbols cannot hold pumps open without end.
pub const MAX_WATCHED: usize = 32;

/// How long the pump of a symbol the door gave up on stays open before [`TradeLeaseRegistry::close_idle_pumps`]
/// closes it: long enough for the operator's retry of a "no price yet" refusal to find it.
pub const IDLE_TTL: Duration = Duration::from_secs(30);

/// How often the reaper thread ([`TradeLeaseRegistry::spawn_idle_reaper`]) looks for idle pumps.
const REAP_EVERY: Duration = Duration::from_secs(5);

/// The most refusals [`TradeLeaseRegistry`] remembers at once (one per distinct refused name).
const MAX_FAILED: usize = 256;

/// **How long the node waits for the gatewayhub's answer to an operator's venue read**
/// ([`VenueReads::venue_read`]): a venue read is one or two signed REST calls at the bridge's own
/// HTTP timeout, so a venue that is slow comes back as the daemon's words well inside this, and one that
/// is silent comes back as the lease's timeout. Held BELOW `vike_tradehub_client::liveness::ONE_SHOT_REPLY_TIMEOUT`
/// (the wait of the client that asked, asserted below) so a slow read reaches the operator as a sentence
/// and not as the client's own timeout.
pub const VENUE_READ_BOUND: Duration = Duration::from_secs(45);

const _: () = assert!(
    VENUE_READ_BOUND.as_secs() < vike_tradehub_client::liveness::ONE_SHOT_REPLY_TIMEOUT.as_secs(),
    "the node must answer an operator's venue read before the one-shot client gives up on it"
);

/// How long an open-orders read waits for the wire ids of the account's own live orders after the venue answered
/// ([`VenueReads::wire_ids`]): a pure function on the account's dispatch thread, so only a queue behind it is slow,
/// and then the rows stay as the venue listed them. Inside the one-shot client's wait together with the read.
pub const WIRE_IDS_BOUND: Duration = Duration::from_secs(5);

const _: () = assert!(
    VENUE_READ_BOUND.as_secs() + WIRE_IDS_BOUND.as_secs()
        < vike_tradehub_client::liveness::ONE_SHOT_REPLY_TIMEOUT.as_secs(),
    "the read and the wire-id join must both fit in the one-shot client's wait"
);

/// How often the lane keeper ([`TradeLeaseRegistry::spawn_lane_keeper`]) reads the published engines for a position or a
/// live order in an engine's own symbol: the longest an out-of-band holding (a reconcile-adopted position, a
/// strategy's order) goes without a lane, and, with [`HOLD_GRACE`] and [`IDLE_TTL`], the delay before a flat
/// engine's lane is closed.
pub const LANE_KEEP_EVERY: Duration = Duration::from_secs(15);

/// How long a held lane survives the engine's last sighting of a position or a live order in the symbol: an
/// order the door has just let through is not in the published snapshot yet, and an engine that closes and
/// reopens must not flap the datahub subscription.
pub const HOLD_GRACE: Duration = Duration::from_secs(30);

/// How long a lane the node could not have (a venue ceiling, a closed market, no feed) waits before the next try.
const HOLD_RETRY_AFTER: Duration = Duration::from_secs(60);

/// **The venues whose own-symbol lane is never held, each with the reason** (decision 0197). Alpaca is the
/// owner's deferral (2026-10-18, no usable key): its price handle exists only when a MOUNT wires it, and
/// opening a credentialed stream for a deferred venue is not this feature's to start.
pub const NO_HELD_LANE: &[(&str, &str)] = &[(
    "alpaca",
    "deferred by the owner to the far future (2026-10-18): the venue has no usable data key, so nothing opens its \
     stream on the node's own initiative",
)];

/// How long an admitted symbol of a venue whose pump goes back when the engine holds nothing in it (A39: its
/// credentialed row's `reaps_idle_admitted`, `vike_gatewayhub_client::credentialed_limits`; everywhere else an
/// admitted symbol's pump is permanent, because a position left unpriced is the worse failure) must have shown the
/// engine holding nothing (no position, no live order) before its pump goes back: long enough that a trader who closes and comes back in
/// a few minutes does not meet the "no price yet, retry" of a pair opened afresh. A door order on the symbol
/// starts the count again at once, because the published snapshot lags the order.
pub const ADMITTED_IDLE_GRACE: Duration = Duration::from_secs(300);

/// How long a requested give-back of an admitted symbol waits for the engine to let it go before the node
/// concludes the engine REFUSED it and drops the request ([`TradeLeaseRegistry::settle_releases`]): the published snapshot
/// lags the core by its coalescing and the tick below, a refusal leaves the symbol listed for ever, and the
/// engine's note in the recent events says why.
pub const RELEASE_WAIT: Duration = Duration::from_secs(60);

/// How long a give-back the engine took but the node could not finish (the gatewayhub did not answer the forget) is
/// tried again before it is given up and said at `warn`: the symbol then stays in this registry's tables and the
/// daemon's reconcile list until the account's engine stops, which costs a slot there, never an order.
pub const RELEASE_RETRY_FOR: Duration = Duration::from_secs(600);

/// How long a combo leg's commit (or forget) the gatewayhub could not answer is asked again, once per tick of the
/// admitted-pair reaper ([`TradeLeaseRegistry::settle_combo_legs`]), before it is given up and said at `warn`: a leg not told
/// is read only from the fills the bridge saw itself, a leg not forgotten keeps a place in the daemon's list.
pub const COMBO_LEG_RETRY_FOR: Duration = Duration::from_secs(600);

/// How long after a combo last named a leg the node waits before it may stop the daemon reading the leg
/// ([`TradeLeaseRegistry::settle_combo_legs`]): the published snapshot lags the core (a combo just sent may not be listed
/// yet), and the venue's late word on a combo that ended in the same instant needs the read. Twice the engine's own
/// quiet minute for a symbol it gives back (`vike_execution::UNADMIT_QUIET_MS`).
pub const COMBO_LEG_QUIET: Duration = Duration::from_secs(120);

/// One combo leg [`TradeLeaseRegistry::tell_combo_legs`] told, or owes, the daemon behind its account's lease.
#[derive(Debug, Clone)]
struct ComboLeg {
    /// The combos (order client ids) that named the leg: while one of them is live, the leg is needed.
    combos: Vec<String>,
    /// When a combo (or the boot) last named the leg: [`COMBO_LEG_QUIET`] counts from here.
    named: Instant,
    /// The daemon granted its commit at least once: only then is there a read to forget.
    told: bool,
    /// Since when its commit is owed (the gatewayhub could not be asked); `None` once told or given up.
    owed: Option<Instant>,
    /// Since when its forget is owed (nothing needs it and the gatewayhub did not answer).
    forgetting: Option<Instant>,
}

/// Where a symbol's price subscription stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sub {
    /// Someone is subscribing right now (the RPCs run outside the lock).
    Opening,
    /// Subscribed, and an order is using it.
    Open,
    /// Subscribed, and the door let go of the symbol at this instant without admitting it.
    Idle(Instant),
    /// The reaper is closing the pump right now (the RPC runs outside the lock).
    Closing,
}

/// Where an engine's hold on the price lane of its OWN symbol stands ([`TradeLeaseRegistry::hold_price`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    /// A thread is opening the lane right now (the RPCs run outside the lock) for the engine as of `generation`.
    Opening { generation: u64 },
    /// The lane is open and wanted; `seen` is the last time the engine was seen holding something in the symbol.
    Open { seen: Instant },
    /// The lane could not be had (said once); not asked again before [`HOLD_RETRY_AFTER`] has passed.
    Refused { at: Instant },
}

/// Where the BAR lane of a mount the boot did not wire stands ([`TradeLeaseRegistry::keep_bars`], decision 0245).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarHold {
    /// A thread is subscribing right now (the datahub round trip runs with no lock held).
    Opening,
    /// Subscribed; the feed's teardown releases it.
    Open,
    /// The lane could not be had (said once); not asked again before [`HOLD_RETRY_AFTER`] has passed.
    Refused { at: Instant },
}

/// The boot re-admission window ([`TradeLeaseRegistry::begin_boot`]).
struct Boot {
    /// What one gatewayhub round trip may take.
    bound: Duration,
    /// The route keys whose lease already missed `bound` in this window: not asked again.
    missed: HashSet<String>,
}

/// Ends the boot window when dropped ([`TradeLeaseRegistry::begin_boot`]).
pub struct BootWindow<'a>(&'a TradeLeaseRegistry);

impl Drop for BootWindow<'_> {
    fn drop(&mut self) {
        *self.0.boot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Ends one retry round's bounded window when dropped ([`TradeLeaseRegistry::begin_round`]).
pub struct RoundWindow<'a>(&'a TradeLeaseRegistry);

impl Drop for RoundWindow<'_> {
    fn drop(&mut self) {
        self.0
            .rounds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&std::thread::current().id());
    }
}

/// What the hot starts and stops of one account's engine left behind.
#[derive(Default)]
struct EngineState {
    /// Bumped by every start and stop of the account's engine ([`TradeLeaseRegistry::generation`]).
    generation: u64,
    /// `Some` once a hot start or stop spoke: whether THAT engine carries an account ceiling (a stopped
    /// one reads `false`, so the door refuses until the next start says otherwise). `None`: the boot's
    /// engine, answered by the boot's rows.
    cap: Option<bool>,
    /// An engine (the boot's or a hot start's) whose venue balance has not landed yet: the cell its reconcile
    /// leg fills and when the start announced it ([`TradeLeaseRegistry::engine_awaiting_balance`]). `None`: nothing waits
    /// (a `paper` account, a start with no reconcile leg, a stopped account).
    awaiting: Option<(Arc<ReconcileFirstBalance>, Instant)>,
    /// The engine was stopped (or its start was refused) and has not been started since: it has no lease, and
    /// no price lane is held for it ([`TradeLeaseRegistry::hold_price`]). The boot's engines have no entry: they run.
    stopped: bool,
}

/// "Does this account (by route key) run with an account-wide exposure ceiling?"
type ExposureCap = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// "Is a strategy mounted on this `(venue, symbol)`?": its feed prices the symbol already.
type MountedPairs = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// "When (epoch ms) was the engine's standing rate for this quote currency of this account (by route key)
/// observed?" `None`: the engine holds no rate row for it.
type RateRows = Box<dyn Fn(&str, &str) -> Option<i64> + Send + Sync>;

/// One venue's share of a rate-timer tick: the symbols it has rated answers for, and the journaled ones nothing
/// has rated yet (`(route key, symbol)` pairs).
type VenueShare = (Vec<(String, String)>, Vec<(String, String)>);

/// Where the timer's heal of one owed symbol stands: how many tries failed in a row, when the next may be made and
/// the ladder that spaces them. Present exactly while the symbol is failing; a try that heals it removes it.
#[derive(Debug, Clone)]
struct OwedBackoff {
    fails: u32,
    next_at: Instant,
    ladder: ReconnectBackoff,
}

impl OwedBackoff {
    /// The state after one more failed try at `now`, given the state before it (`None`: this is the first failure).
    /// The wait doubles with each failure from `first` and never exceeds [`OWED_RETRY_CAP_FACTOR`] times it (the one
    /// ladder, `vike_venues::reconnect::ReconnectBackoff`).
    fn failed(before: Option<Self>, now: Instant, first: Duration) -> Self {
        let (fails, mut ladder) = before.map_or_else(
            || (0, ReconnectBackoff::new(first, first.saturating_mul(OWED_RETRY_CAP_FACTOR))),
            |b| (b.fails, b.ladder),
        );
        Self { fails: fails.saturating_add(1), next_at: now + ladder.failed(), ladder }
    }

    /// Whether the next try may be made at `now`.
    fn due(&self, now: Instant) -> bool {
        now >= self.next_at
    }
}

/// The timer's `held` question, `(route key, symbol)` to "does the account still have something in it", shared by
/// the venues' rate workers.
type HeldCheck = dyn Fn(&str, &str) -> bool + Send + Sync;

/// What a venue's long-lived rate worker is, from the timer's side ([`RatePool`]).
struct RateWorker {
    /// The next share of the venue's symbols to read. Dropping it ends the worker.
    jobs: std::sync::mpsc::Sender<VenueShare>,
    /// Whether a share is being read and whether a tick was skipped meanwhile ([`Handover`]).
    state: Arc<Mutex<Handover>>,
}

/// The hand-over between the timer and one venue's worker, under ONE lock: with `busy` and `missed` as two atomics a
/// tick that saw `busy` just before the worker cleared it set `missed` after the worker had last looked, and the
/// skipped tick was never caught up (the venue then waited a whole period more, and the engine calls a rate stale at
/// two).
#[derive(Default)]
struct Handover {
    /// A share is being read: set by the timer when it hands one over, cleared by the worker when it is done.
    busy: bool,
    /// A tick came while `busy`: the worker reads its share once more, at once, when it is done.
    missed: bool,
}

/// What [`RatePool::hand_over`] did with a venue's share.
enum Handed {
    /// The venue's worker has it.
    Queued,
    /// The worker is still reading the share of an earlier tick: this one is skipped (and the worker told).
    Busy,
    /// No worker could be had (the OS refused a thread, or the worker was gone): the share comes back, and the
    /// timer reads it on its own thread.
    Failed(VenueShare),
}

/// **One long-lived rate worker per venue, and the channel their results come back on** (A53). The rate timer
/// ([`TradeLeaseRegistry::spawn_rate_refresher`]) used to read every venue's symbols on scoped threads it joined before the
/// next tick, so one slow venue (an IBKR gateway that takes seconds per quiet pair) held the NEXT tick of every
/// venue behind it. Now the timer only hands a venue's share to that venue's worker ([`Self::hand_over`]) and goes
/// on; the worker reads it ([`TradeLeaseRegistry::refresh_venue`]) and puts the commands on `done`, which the timer sends
/// on as they arrive ([`Self::send_finished_until`]). A worker is started at a venue's first share and lives until
/// the registry is dropped; it holds only a `Weak`.
struct RatePool {
    weak: std::sync::Weak<TradeLeaseRegistry>,
    held: Arc<HeldCheck>,
    workers: HashMap<String, RateWorker>,
    done_tx: std::sync::mpsc::Sender<Vec<Command>>,
    done_rx: std::sync::mpsc::Receiver<Vec<Command>>,
}

impl RatePool {
    fn new(weak: std::sync::Weak<TradeLeaseRegistry>, held: Arc<HeldCheck>) -> Self {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        Self { weak, held, workers: HashMap::new(), done_tx, done_rx }
    }

    /// Hand `venue`'s `share` to its worker, starting the worker at the venue's first share.
    fn hand_over(&mut self, venue: &str, share: VenueShare) -> Handed {
        if !self.workers.contains_key(venue) {
            match self.start_worker(venue) {
                Ok(worker) => {
                    self.workers.insert(venue.to_string(), worker);
                }
                Err(e) => {
                    tracing::warn!(
                        %venue,
                        "could not start the conversion-rate worker of {venue} ({e}); this tick reads its share \
                         on the timer's own thread"
                    );
                    return Handed::Failed(share);
                }
            }
        }
        let worker = &self.workers[venue];
        {
            let mut state = worker.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.busy {
                state.missed = true;
                return Handed::Busy;
            }
            // The worker reads this share once, so a catch-up asked by an earlier skip is answered by it.
            *state = Handover { busy: true, missed: false };
        }
        match worker.jobs.send(share) {
            Ok(()) => Handed::Queued,
            // The worker is gone (it ended with the registry): forget it, the next tick starts another.
            Err(std::sync::mpsc::SendError(share)) => {
                self.workers.remove(venue);
                Handed::Failed(share)
            }
        }
    }

    fn start_worker(&self, venue: &str) -> std::io::Result<RateWorker> {
        let (jobs, inbox) = std::sync::mpsc::channel();
        let state = Arc::new(Mutex::new(Handover::default()));
        let worker = RateWorkerThread {
            venue: venue.to_string(),
            weak: self.weak.clone(),
            held: Arc::clone(&self.held),
            inbox,
            state: Arc::clone(&state),
            done: self.done_tx.clone(),
        };
        std::thread::Builder::new()
            .name(format!("lease-facts-rates:{venue}"))
            .spawn(move || worker.run())?;
        Ok(RateWorker { jobs, state })
    }

    /// Send on, the moment each arrives, what the workers have read, until `deadline`.
    fn send_finished_until(&self, deadline: Instant, send: &mut dyn FnMut(Command)) {
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match self.done_rx.recv_timeout(wait) {
                Ok(commands) => commands.into_iter().for_each(&mut *send),
                // `self` holds a sender, so the channel cannot close; this is the deadline.
                Err(_) => return,
            }
        }
    }
}

/// What a venue's rate worker thread owns.
struct RateWorkerThread {
    venue: String,
    weak: std::sync::Weak<TradeLeaseRegistry>,
    held: Arc<HeldCheck>,
    inbox: std::sync::mpsc::Receiver<VenueShare>,
    state: Arc<Mutex<Handover>>,
    done: std::sync::mpsc::Sender<Vec<Command>>,
}

impl RateWorkerThread {
    /// Read each share the timer hands over, put the commands on `done`, and read the same share once more at once
    /// when a tick came meanwhile ([`Handover::missed`]). Ends when the registry is dropped or the pool is.
    fn run(self) {
        while let Ok((rated, owed)) = self.inbox.recv() {
            loop {
                let Some(facts) = self.weak.upgrade() else { return };
                // A panic costs this read, not the worker: its symbols keep the rate they have, and the next tick
                // tries again (a worker that died would leave `busy` set for ever).
                let commands = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    facts.refresh_venue(&self.venue, &rated, &owed, &*self.held)
                }))
                .unwrap_or_else(|_| {
                    tracing::error!(
                        venue = %self.venue,
                        "a conversion-rate worker panicked; its symbols keep the rate they have"
                    );
                    Vec::new()
                });
                drop(facts);
                if self.done.send(commands).is_err() {
                    return;
                }
                // Looked at and cleared under the timer's lock: a tick skipped meanwhile is caught up, and a tick
                // that comes after `busy` is cleared is handed over (not skipped).
                let again = {
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    let again = std::mem::take(&mut state.missed);
                    state.busy = again;
                    again
                };
                if !again {
                    break;
                }
            }
        }
    }
}

/// How old a held foreign-quoted position's rate may get before the timer says it is NOT being refreshed: past
/// the timer's own cadence and its retry ([`RATE_REFRESH_EVERY`] and a half), short of the engine's 120 s
/// staleness, so the line comes up while the engine is about to deny opening orders, not after.
pub const RATE_UNREFRESHED_AFTER: Duration = Duration::from_secs(90);

/// One held foreign-quoted position whose conversion rate the timer could not keep fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrefreshed {
    /// The account.
    pub route_key: String,
    /// The symbol.
    pub symbol: String,
    /// The currency it is quoted in.
    pub currency: String,
    /// How old the engine's rate for that currency is, in seconds; `None`: the engine holds no row for it.
    pub age_s: Option<i64>,
}

/// How often the mount rate timer's tick looks at the mounts it keeps rated ([`TradeLeaseRegistry::spawn_mount_rater`]): a
/// mount noted at a start is read at the next tick, so the engine's first rate for it lands within seconds of the
/// start, not a [`RATE_REFRESH_EVERY`] later. A tick that finds nothing due reads nothing.
pub const MOUNT_RATE_TICK: Duration = Duration::from_secs(2);

/// How long after a successful read the mount rate timer reads a mount again: inside the engine's 120 s staleness
/// window with room for one failed read and its retry ([`RATE_RETRY_AFTER`]), and tighter than the timer of a HELD
/// position's rate ([`RATE_REFRESH_EVERY`]) because the mount is the engine's own symbol: a strategy mounted on it
/// and the operator's orders on it have no other source of its rate.
pub const MOUNT_RATE_EVERY: Duration = Duration::from_secs(40);

/// How long after its venue's market opens a paper account's missing demo login is still that login coming up, not a
/// fault ([`TradeLeaseRegistry::state_paper_units`]): the demo account is started at the open
/// (`crate::account_follow::fx_retry_due`) and its login takes seconds, while the stater ticks every
/// [`MOUNT_RATE_TICK`].
const PAPER_UNITS_OPEN_GRACE: Duration = Duration::from_secs(120);

/// The engine's mounted symbol that is booked through a rate ([`TradeLeaseRegistry::note_foreign_mount`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MountQuote {
    /// The venue of the account.
    venue: String,
    /// The symbol the engine was started on.
    symbol: String,
    /// The currency it is quoted in (not the ledger's).
    currency: String,
}

/// How long a kept answer is served: [`FACTS_TTL`], or [`RATE_FRESH_FOR`] when it carries a conversion rate.
fn kept_for(admission: &SymbolAdmission) -> Duration {
    if admission.quote_rate.is_some() { RATE_FRESH_FOR } else { FACTS_TTL }
}

/// How long a kept answer waits before an order re-reads it: [`REFRESH_AFTER`], or [`RATE_FRESH_FOR`] when
/// it carries a conversion rate.
fn refresh_after(admission: &SymbolAdmission) -> Duration {
    if admission.quote_rate.is_some() { RATE_FRESH_FOR } else { REFRESH_AFTER }
}

/// How long the gatewayhub's word that a venue path is up stands with no newer one: three asks, so one slow answer does
/// not read as not knowing.
const VENUE_PATH_UP_FRESH: Duration =
    Duration::from_secs(super::venue_link::ASK_EVERY.as_secs() * 3);

/// The most characters of a link's or a refusal's words one frame row carries.
const FRAME_WORDS_LIMIT: usize = 400;

/// A peer's or a bridge's words as an observe frame carries them: bounded, on one line.
pub(super) fn frame_words(text: &str) -> String {
    text.chars().take(FRAME_WORDS_LIMIT).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// See the module doc.
#[derive(Default)]
pub struct TradeLeaseRegistry {
    sessions: Mutex<HashMap<String, TradeLease>>,
    /// `(route key, leg)` -> a combo leg this registry told, or owes, the daemon ([`Self::tell_combo_legs`]), until
    /// nothing needs it ([`Self::settle_combo_legs`]) or its engine stops.
    combo_legs: Mutex<HashMap<(String, String), ComboLeg>>,
    answers: Mutex<HashMap<(String, String), (Instant, SymbolAdmission)>>,
    /// Route key -> the symbol its start mounted and the grid the start stated for it ([`Self::note_own_grid`]).
    own_grids: Mutex<HashMap<String, (String, SymbolGrid)>>,
    /// Each venue's feed handle, bound once the feeds are wired.
    prices: OnceLock<HashMap<String, StrategyLaneCexWatcher>>,
    /// `(venue, symbol)` -> the state of its price subscription.
    watched: Mutex<HashMap<(String, String), Sub>>,
    /// `(route key, symbol)` pairs the door (or the boot) sent `AdmitSymbol` for.
    admitted: Mutex<HashSet<(String, String)>>,
    /// `(route key, symbol)` pairs a started lane twin warmed ([`Self::warm_price`]), until its engine stops:
    /// the twin's own symbol is never admitted, so [`Self::engine_stopped`] would not find its pump in
    /// [`Self::admitted`] and the pump would hold its place and its datahub subscription to the session's end.
    warmed: Mutex<HashSet<(String, String)>>,
    /// `(route key, own symbol)` -> where an engine's (default or labelled) hold on its own symbol's price lane stands
    /// ([`Hold`]): kept while the engine holds a position or a live order in the symbol, handed to the idle
    /// reaper when it does not. Not counted under [`MAX_WATCHED`]: the lane is the engine's.
    held: Mutex<HashMap<(String, String), Hold>>,
    /// This registry's own handle, set by [`Self::spawn_lane_keeper`]: what lets [`SymbolAdmitter::hold_lane`] (a
    /// `&self` call on the control door) open a lane on a thread of its own instead of on the order's.
    me: OnceLock<std::sync::Weak<TradeLeaseRegistry>>,
    /// Whether a strategy is mounted on a `(venue, symbol)`, bound by the root ([`Self::bind_mounted`]): the
    /// mount's feed holds that lane (the gatewayhub allows one tick pump per symbol per session, so a second
    /// subscription of the symbol on the same dial would be refused).
    mounted: OnceLock<MountedPairs>,
    /// The `(venue, symbol)` pairs whose tap mount mark is switched on right now ([`Self::sync_mount_marks`]).
    mount_marks: Mutex<HashSet<(String, String)>>,
    /// The `(venue, symbol)` pairs whose quotes the tap folds into a paper book's bars right now
    /// ([`Self::sync_paper_quotes`]).
    paper_bars: Mutex<HashSet<(String, String)>>,
    /// The route keys of the paper engines this registry says have no demo login to be priced through
    /// ([`Self::sync_paper_quotes`]).
    paper_unpriced: Mutex<HashSet<String>>,
    /// The bar series the boot's mounts subscribed ([`Self::bind_boot_bars`]): [`Self::keep_bars`] never asks for
    /// one of them again.
    boot_bars: OnceLock<HashSet<SeriesKey>>,
    /// The bar lanes opened for mounts the boot did not wire ([`Self::keep_bars`]) and for price alerts
    /// (`SymbolAdmitter::keep_bars`), kept to the process's end.
    bars: Mutex<HashMap<SeriesKey, BarHold>>,
    /// The keyless facts look-up an account with NO trade lease asks ([`Self::bind_keyless_facts`]): a paper account
    /// never logs in, so its symbol's facts come from the datahub's strategy lane (`StrategySymbolFacts`).
    keyless: OnceLock<KeylessFacts>,
    /// The series the mount keeper last asked for ([`Self::keep_bars`]): only a lane of one of them counts as a
    /// paper book's price source ([`SymbolAdmitter::bar_series`]). An alert's lane carries candles and no quotes, so
    /// a paper order it admitted would fill on a bar's open from before the order, on no side.
    mount_bars: Mutex<HashSet<SeriesKey>>,
    /// `(route key, symbol)` -> the leverage the venue was SEEN holding for the symbol at the last COMMIT
    /// to the daemon (`TradeLease::admit_symbol_confirmed`): set on a figure, removed when the commit
    /// confirmed none. The facts LOOK reads it (`lookup`), so a refresh sent later (every
    /// [`REFRESH_AFTER`]) restates the symbol's margin rate instead of dropping it; only a commit writes
    /// it, because only a commit may ask the exchange anything.
    leverage: Mutex<HashMap<(String, String), f64>>,
    /// Route key -> the currency the VENUE stated for the account when its adapter came up (`AdapterReply::Running`'s
    /// `account_currency`), which the engine's ledger is anchored to for an account that converts. A symbol quoted in
    /// another currency is admitted only for a route key that has one: every rate the engine is then handed is
    /// expressed in that currency, and an engine whose ledger label is a default nobody agreed to would be
    /// handed rates in a currency it does not know ([`Self::anchor_currency`]).
    anchored: Mutex<HashMap<String, String>>,
    /// The route keys whose [`Self::anchored`] currency came from the JOURNAL and that no rate can confirm
    /// (the venue's rates do not state their ledger: its `vike_venues::venue_currency` row's `rate_ledger_stamp` is
    /// `Optional`): a symbol quoted in another
    /// currency is NOT admitted on such an anchor ([`Self::anchor_unconfirmed`]).
    unconfirmed: Mutex<HashSet<String>>,
    /// Route key -> why the account's currency was NOT anchored although the venue (or the journal) stated one: the
    /// mount fence's words ([`Self::note_unanchorable_mount`]). A refusal of a symbol quoted in another currency says
    /// them, instead of "the currency is not known".
    unanchorable: Mutex<HashMap<String, String>>,
    /// Route key -> the engine's MOUNTED symbol and the currency it is quoted in, for an account whose ledger is
    /// not in that currency and whose venue rates the mount (decision 0183, the mounted-symbol amendment;
    /// [`Self::note_foreign_mount`]). The engine DECLARES such a mount at assembly and books it through a rate;
    /// this is what the node keeps rated ([`Self::rate_foreign_mounts`]) and re-rates at the door
    /// ([`SymbolAdmitter::own_symbol_rate`]). Replaced by every start, dropped with the lease and the engine.
    mounts: Mutex<HashMap<String, MountQuote>>,
    /// Route key -> when the mount rate timer may read that mount again and how many reads failed in a row
    /// ([`Self::rate_foreign_mounts`]): absent means due now (a mount just noted).
    mount_next: Mutex<HashMap<String, (Instant, u32)>>,
    /// Route key -> the short reason the account's balance gate holds its venue balance back ([`Self::set_balance_held`]),
    /// for the operator surfaces ([`AccountReadiness::balance_held`]); written by the gate itself (`balance_gate`), so it
    /// follows what the gate does and not what the start decided.
    held_balances: Mutex<BTreeMap<String, String>>,
    /// Route key -> the flag that lifts the hold of an engine bound to an ASSUMED currency
    /// ([`Self::note_assumed_hold`]): raised by the first facts answer that states the engine's anchor as the VENUE's
    /// own word ([`Self::confirm_assumed_anchor`]), read by the account's balance gate. Replaced by every start,
    /// dropped with the lease and the engine.
    assumed_holds: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Route key -> the account's venue path while it is DOWN ([`Self::set_venue_link_down`]), as the gatewayhub's
    /// status report last said it, for the observe frame ([`LinkHealth::venue_link_down`]); written by the lease
    /// worker's poll (`reteach`), cleared with the lease.
    venue_links: Mutex<BTreeMap<String, VenueLinkDownWire>>,
    /// Route key -> when the gatewayhub last said the account's venue path is up ([`Self::note_venue_path_up`]). Only a
    /// fresh answer is stamped ([`VENUE_PATH_UP_FRESH`]): an ask that stopped being answered says nothing any more.
    venue_paths_up: Mutex<BTreeMap<String, Instant>>,
    /// Route key -> when the account's trade link went down and why ([`Self::set_trade_link_lost`]), written on the
    /// link thread. Only the words and the instant: whether a lease is up at all is the session's own state, read at
    /// the stamp ([`LinkHealth::trade_link_down`]).
    trade_links: Mutex<HashMap<String, (Instant, String)>>,
    /// The price feeds' dials ([`Self::bind_price_links`]); unset on a node with none.
    price_links: OnceLock<Arc<PriceLinks>>,
    /// Route key -> why the account's last start was refused ([`Self::set_start_refusal`]): the cause behind the one
    /// word `adapter failed`, which `vike_execution::ArmingNotRunningView` cannot carry (it is in the core's closure).
    start_refusals: Mutex<HashMap<String, String>>,
    /// Route key -> the `(symbol, quote currency)` pairs the JOURNAL says the account's restored engine held
    /// quoted in another currency ([`Self::note_journaled_foreign`]). A restored position's rate is as old as the
    /// snapshot; the rate timer keeps these symbols' rates fresh even when the boot's re-admission missed them
    /// (no answer is cached for them, so [`Self::rated_symbols`] does not list them), and says so when it cannot.
    journaled: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// The age of the engine's standing rate for `(route key, currency)`, bound by the root from the core's
    /// snapshot ([`Self::bind_rate_rows`]): what the timer's "this rate is not being refreshed" line reads.
    rate_rows: OnceLock<RateRows>,
    /// `(route key, symbol)` -> when the gatewayhub last REFUSED the symbol (unknown, no tick size, ...), and
    /// why: not asked again for [`NEGATIVE_TTL`] (a flood of orders must not become a flood of lookups).
    /// A link failure or a busy daemon is never entered.
    failed: Mutex<HashMap<(String, String), (Instant, SymbolPropertiesRequestError)>>,
    /// Whether an account has an account-wide exposure ceiling armed, bound by the root from the
    /// policy and the account rows.
    exposure: OnceLock<ExposureCap>,
    /// Route key -> what the HOT starts and stops of the account's engine left ([`EngineState`]). The
    /// cap outranks [`Self::exposure`], which describes the boot's rows: the door must answer for the
    /// engine that actually runs. Cap and generation share ONE lock, so they change together.
    engines: Mutex<HashMap<String, EngineState>>,
    /// The bound a boot round trip gets, set once by the backend that knows
    /// `config.gatewayhub_connect_wait_ms` ([`Self::set_boot_wait`]).
    boot_wait: OnceLock<Duration>,
    /// `Some` while the boot re-admission runs ([`Self::begin_boot`]).
    boot: Mutex<Option<Boot>>,
    /// The retry rounds in flight, by the thread that runs each ([`Self::begin_round`]): the boot's
    /// bound and `missed` short-circuit for THAT thread alone, so the door's own lookups keep the
    /// lease's `request_reply`.
    rounds: Mutex<HashMap<std::thread::ThreadId, Boot>>,
    /// An override of [`IDLE_TTL`] (tests).
    idle_ttl: OnceLock<Duration>,
    /// `(route key, symbol)` of an ADMITTED symbol of a venue that reaps one (`credentialed_limits::reaps_idle_admitted`) -> when the reaper first saw
    /// its engine hold nothing in it ([`Self::reap_idle_admitted`]); absent while the engine holds something, or the
    /// door has just asked for the symbol.
    idle_admitted: Mutex<HashMap<(String, String), Instant>>,
    /// `(route key, symbol)` -> when the door sent the engine a statement that GIVES the symbol back
    /// ([`SymbolAdmitter::release_requested`], decision 0205), for [`Self::settle_releases`] to finish once the
    /// published snapshot shows the engine no longer lists it. A re-admission of the symbol drops the entry.
    releasing: Mutex<HashMap<(String, String), Instant>>,
    /// `(venue, symbol)` pumps of admitted symbols the reaper closed -> when re-opening one last failed (`None`: it
    /// has not been tried). Cleared by the next subscribe of the pump, whoever makes it.
    reaped: Mutex<HashMap<(String, String), Option<Instant>>>,
    /// An override of [`ADMITTED_IDLE_GRACE`] (tests).
    admitted_idle_grace: OnceLock<Duration>,
    /// `(route key, symbol)` -> where the rate timer's heal of an owed symbol stands ([`OwedBackoff`]).
    owed_backoff: Mutex<HashMap<(String, String), OwedBackoff>>,
    /// An override of [`OWED_RETRY_FIRST`] (tests).
    owed_retry_first: OnceLock<Duration>,
    /// What the account watcher knows about lane twins that a published block cannot say
    /// ([`Self::set_lane_twin_flags`]).
    twin_flags: Mutex<TwinFlags>,
    /// `(route key, symbol)` of a PAPER engine whose own symbol's contract value comes from the venue's demo login
    /// -> its last try ([`Self::state_paper_units`]).
    paper_units: Mutex<HashMap<(String, String), PaperUnitTry>>,
}

/// One try of [`TradeLeaseRegistry::state_paper_units`] for a paper engine's own symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaperUnitTry {
    /// The engine generation it was made for ([`TradeLeaseRegistry::generation`]).
    generation: u64,
    /// Why it failed; `None`: the contract value was stated to that engine.
    failed: Option<String>,
}

/// The account watcher's last word on the lane twins ([`TradeLeaseRegistry::set_lane_twin_flags`]), both as route keys.
#[derive(Default)]
struct TwinFlags {
    /// Accounts that are an operator's REAL ones and spell a lane twin's label (no twin is derived for them).
    real: HashSet<String>,
    /// Lane twins kept up only to be closed: their row is gone and they still hold something.
    closing: HashSet<String>,
    /// Lane twins the node DERIVED from the rows and RUNS ([`TradeLeaseRegistry::set_derived_lane_twins`]): the positive
    /// evidence a `live` engine needs before the door treats it as the twin.
    derived: HashSet<String>,
    /// Every account the rows configure, by venue, or why they could not be read
    /// ([`TradeLeaseRegistry::set_configured_accounts`]). `None` until the watcher's first read.
    configured: Option<Result<BTreeMap<String, Vec<String>>, String>>,
}

/// Whether an engine built from `rows` for `(venue, label)` carries an account-wide exposure ceiling:
/// the box-wide policy row (`policy_cap`), or the account's own `max_exposure` row (the engine takes
/// the tighter of the two, so either one means a ceiling). Rows that were not read, or could not be,
/// arm nothing: the engine reads the same rows the same way, so the door's answer is the engine's.
#[must_use]
pub fn rows_arm_a_cap(
    policy_cap: bool,
    rows: &AccountDirectory,
    venue: &str,
    label: &AccountLabel,
) -> bool {
    policy_cap
        || rows
            .rows()
            .and_then(Result::ok)
            .and_then(|rows| rows.max_exposure_of(venue, label.text()))
            .is_some()
}

/// **Which running lease is `venue`'s DEMO LOGIN, the one its price stream rides** (`login`,
/// `crate::feeds::paper_price_login`, the venue's credentialed row in `vike_gatewayhub_client::credentialed_limits`): for a
/// stream that logs in as the venue's DEFAULT account (ibkr's IB Gateway session), that account's own `demo` lease and no
/// other, so the order check and the stream always agree; for one that rides whatever session a running demo account
/// holds in the gatewayhub process (dukascopy's sidecar, fxcm's reconcile session), any `demo` lease of the venue, the
/// lowest route key so the answer is stable. `None` for any other venue (a keyless stream logs in to nothing), and when
/// no such lease runs. `leases` is `(route key, venue, tier)` of every open lease. Pure.
pub(crate) fn pick_demo_login<'a>(
    login: Option<CredentialedStreamLogin>,
    venue: &str,
    leases: impl Iterator<Item = (&'a str, &'a str, vike_model::AccountTier)>,
) -> Option<String> {
    let mut demo = leases
        .filter(|(_, v, tier)| *v == venue && *tier == vike_model::AccountTier::Demo)
        .map(|(route_key, _, _)| route_key);
    match login? {
        CredentialedStreamLogin::DefaultAccount => {
            demo.find(|route_key| *route_key == venue).map(str::to_string)
        }
        CredentialedStreamLogin::AnyDemoSession => demo.min().map(str::to_string),
    }
}

/// `symbol`'s tick size on `venue`'s KEPT instrument list (`vike_catalog::kept_row_of`: the row whose STORED user name
/// it is, decision 0290), or `None` when no list is held, the list has no such row, or the row states no tick.
fn kept_tick(venue: &str, symbol: &str) -> Option<f64> {
    vike_catalog::kept_row_of(venue, symbol)
        .map(|row| row.properties.tick_size)
        .filter(|tick| tick.is_finite() && *tick > 0.0)
}

/// The keyless facts look-up ([`TradeLeaseRegistry::bind_keyless_facts`]): for `(route_key, venue, symbol)`, the
/// symbol's facts when the route key is a `paper` account's, `None` for any other account. A failure says what it
/// DID (`Refused`: the datahub's answer; any other word: a link that carried none) and its text.
type KeylessFacts = Box<
    dyn Fn(
            &str,
            &str,
            &str,
        ) -> Option<Result<vike_model::SymbolProperties, (FailureOutcome, String)>>
        + Send
        + Sync,
>;

/// What [`TradeLeaseRegistry::claim_pump`] decided for a symbol the door asks a price of.
enum Claim {
    /// The caller subscribes the pump.
    Subscribe,
    /// A pump is open (or opening): read the price.
    Held,
    /// The reaper is closing the pump: no price now, the retry subscribes afresh.
    Closing,
}

/// Remember a refusal of `key`, keeping the map within [`MAX_FAILED`]: a full map first drops what has
/// expired, and a refusal that still does not fit is simply not remembered (the next order asks again).
fn remember_refusal(
    failed: &mut HashMap<(String, String), (Instant, SymbolPropertiesRequestError)>,
    key: (String, String),
    why: SymbolPropertiesRequestError,
) {
    if failed.len() >= MAX_FAILED && !failed.contains_key(&key) {
        failed.retain(|_, (at, _)| at.elapsed() < NEGATIVE_TTL);
    }
    if failed.len() < MAX_FAILED || failed.contains_key(&key) {
        failed.insert(key, (Instant::now(), why));
    }
}

impl TradeLeaseRegistry {
    /// Tell the registry the bound a boot round trip gets: `config.gatewayhub_connect_wait_ms`, the bound
    /// of `VenueBackendGateway::start_adapter` (set once, by the backend that was given it).
    pub fn set_boot_wait(&self, wait: Duration) {
        let _ = self.boot_wait.set(wait);
    }

    /// **Open the boot window**: until the guard drops, every gatewayhub round trip of this registry (the
    /// facts look, the admission commit) waits at most the boot bound, and a lease that missed it is not
    /// asked again; the miss is an `Unavailable` answer, so the symbol is NOT admitted (fail closed) and
    /// nothing is remembered about it. A registry told no bound ([`Self::set_boot_wait`]) opens nothing.
    pub fn begin_boot(&self) -> BootWindow<'_> {
        if let Some(&bound) = self.boot_wait.get() {
            *self.boot.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(Boot { bound, missed: HashSet::new() });
        }
        BootWindow(self)
    }

    /// **Open one retry round** of the boot's missed symbols, for the CALLING thread only: its
    /// gatewayhub round trips wait at most the boot bound and a lease that missed it is not asked again
    /// until the guard drops (a silent lease costs one bound per round, not one per symbol). Every
    /// other thread, the door's included, keeps the lease's own `request_reply`. A registry told no
    /// bound opens nothing.
    pub fn begin_round(&self) -> RoundWindow<'_> {
        if let Some(&bound) = self.boot_wait.get() {
            self.rounds
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(std::thread::current().id(), Boot { bound, missed: HashSet::new() });
        }
        RoundWindow(self)
    }

    /// The bound one round trip of a boot window or a retry round gets (zero when none was told).
    pub fn round_bound(&self) -> Duration {
        self.boot_wait.get().copied().unwrap_or_default()
    }

    /// One gatewayhub round trip over `route_key`'s lease: `call` gets the bound of the boot window, or of
    /// the calling thread's retry round, while one is open; `None` (the lease's own `request_reply`)
    /// otherwise.
    fn ask<T>(
        &self,
        route_key: &str,
        call: impl FnOnce(Option<Duration>) -> Result<T, SymbolPropertiesRequestError>,
    ) -> Result<T, SymbolPropertiesRequestError> {
        let me = std::thread::current().id();
        let window = |boot: &Boot| -> Result<Duration, SymbolPropertiesRequestError> {
            if boot.missed.contains(route_key) {
                return Err(SymbolPropertiesRequestError::Unavailable(format!(
                    "boot: `{route_key}`'s trade lease did not answer within {:?} for an earlier \
                     symbol, so it is not asked again before the node is ready",
                    boot.bound
                )));
            }
            Ok(boot.bound)
        };
        let looked: Option<Result<Duration, SymbolPropertiesRequestError>>;
        let in_round = {
            let rounds = self.rounds.lock().unwrap_or_else(PoisonError::into_inner);
            looked = rounds.get(&me).map(window);
            looked.is_some()
        };
        let looked = if in_round {
            looked
        } else {
            self.boot.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(window)
        };
        let bound = looked.transpose()?;
        let started = Instant::now();
        let answer = call(bound);
        if let Some(bound) = bound
            && answer.as_ref().is_err_and(SymbolPropertiesRequestError::is_transient)
            && started.elapsed() >= bound
        {
            if in_round {
                if let Some(round) =
                    self.rounds.lock().unwrap_or_else(PoisonError::into_inner).get_mut(&me)
                {
                    round.missed.insert(route_key.to_string());
                }
            } else if let Some(boot) =
                self.boot.lock().unwrap_or_else(PoisonError::into_inner).as_mut()
            {
                boot.missed.insert(route_key.to_string());
            }
        }
        answer
    }

    /// Remember `session` as the open lease of the account `route_key` names (a later start of the
    /// same account replaces it).
    pub fn register(&self, route_key: &str, session: TradeLease) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(route_key.to_string(), session.clone());
        // Only a lease that is up starts clean of a word about an older one: one whose link dropped since it opened
        // keeps the words its loss noted (the link thread notes them before the lease reads down, and again after).
        let mut lost = self.trade_links.lock().unwrap_or_else(PoisonError::into_inner);
        if session.link_state() == TradeLinkState::Up {
            lost.remove(route_key);
        }
        drop(lost);
        // A remembered lookup failure was about the OLD link: the new lease asks afresh.
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _), _| key != route_key);
    }

    /// Remember the grid a start of `route_key` stated for the `symbol` it mounted: the engine's own figures, known
    /// with no venue asked, so the door can judge an order on the mounted symbol ([`SymbolAdmitter::held_grid`]). A
    /// start that stated none leaves nothing held.
    pub fn note_own_grid(
        &self,
        route_key: &str,
        symbol: &str,
        grid: Option<&vike_model::SymbolProperties>,
    ) {
        let mut own = self.own_grids.lock().unwrap_or_else(PoisonError::into_inner);
        match grid {
            Some(grid) => own.insert(
                route_key.to_string(),
                (symbol.to_string(), SymbolGrid::from_properties(grid)),
            ),
            None => own.remove(route_key),
        };
    }

    /// **Forget the lease a REFUSED start registered** ([`Self::register`]): the start came up, the shared tail
    /// refused it, and its session was detached, so nothing may be asked of it and the currency it stated is
    /// void. Not [`Self::engine_stopped`]: no engine ever existed, so no symbol was admitted and no generation
    /// moves (the watcher's `start_with_cap` already moved it and put the cap answer back).
    pub fn forget_lease(&self, route_key: &str) {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.own_grids.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.anchored.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.unconfirmed.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.unanchorable.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.forget_foreign_mount(route_key);
        self.set_balance_held(route_key, None);
        self.assumed_holds.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.set_venue_link_down(route_key, None);
        self.note_venue_path_up(route_key, false);
        self.set_trade_link_lost(route_key, None);
    }

    /// **Register the flag that lifts `route_key`'s balance hold** (`balance_gate::GateSpec::lift`): the engine was
    /// bound to an ASSUMED currency, so its venue cash is held back, and the first facts answer that states the
    /// engine's anchor as the VENUE's own word ([`Self::confirm_assumed_anchor`]) raises `lift` and ends the hold
    /// without restarting the account. Called by `VenueBackendGateway::start_adapter` after [`Self::anchor_currency`]
    /// (which forgets the previous start's), for an `Assumed` binding only.
    pub fn note_assumed_hold(&self, route_key: &str, lift: Arc<AtomicBool>) {
        self.assumed_holds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(route_key.to_string(), lift);
    }

    /// **The venue has now NAMED the currency the engine only assumed**: a facts answer that carries the proof
    /// (`SymbolPropertiesWire::account_currency_stated`) for the very currency the account's ledger is anchored to raises the
    /// hold's lift flag ([`Self::note_assumed_hold`]). The anchor and the stated currency must be equal: an answer in
    /// ANOTHER currency lifts nothing (the ledger is mislabelled, and the symbol is refused in words below), and a
    /// stamp without the proof (the lane's own assumed USD stamps `USD` too) lifts nothing either.
    fn confirm_assumed_anchor(
        &self,
        route_key: &str,
        venue: &str,
        facts: &vike_gatewayhub_client::proto::SymbolPropertiesWire,
    ) {
        if !facts.account_currency_stated {
            return;
        }
        let Some(stated) = facts.account_currency.as_deref().map(str::trim) else { return };
        let Some(lift) = self
            .assumed_holds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .cloned()
        else {
            return;
        };
        let anchored =
            self.anchored.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned();
        if anchored.is_some_and(|anchor| vike_model::same_currency(&anchor, stated))
            && !lift.swap(true, Ordering::SeqCst)
        {
            tracing::info!(
                %route_key, %venue, currency = %stated,
                "the venue named the account's currency ({stated}), the one the engine's ledger only assumed: \
                 its held balance is released at the next reconcile pass"
            );
        }
    }

    /// **Say that `route_key`'s venue path is down** (`Some`: the last report of the gatewayhub's status verb) or
    /// that it is not (`None`: it reports up, reports nothing, or the lease is gone). Shown as given by
    /// [`LinkHealth::venue_link_down`].
    pub fn set_venue_link_down(&self, route_key: &str, down: Option<VenueLinkDownWire>) {
        let mut links = self.venue_links.lock().unwrap_or_else(PoisonError::into_inner);
        match down {
            Some(down) => {
                links.insert(route_key.to_string(), down);
            }
            None => {
                links.remove(route_key);
            }
        }
    }

    /// [`Self::set_venue_link_down`] for the watch's own "no live adapter": a refusal that already said why keeps its words.
    pub fn note_no_adapter(&self, route_key: &str, down: VenueLinkDownWire) {
        let mut links = self.venue_links.lock().unwrap_or_else(PoisonError::into_inner);
        if !links.get(route_key).is_some_and(|said| said.no_adapter) {
            links.insert(route_key.to_string(), down);
        }
    }

    /// **Say that the gatewayhub just said `route_key`'s venue path is up**, or (`false`) that nothing says so any more.
    pub fn note_venue_path_up(&self, route_key: &str, up: bool) {
        let mut heard = self.venue_paths_up.lock().unwrap_or_else(PoisonError::into_inner);
        if up {
            heard.insert(route_key.to_string(), Instant::now());
        } else {
            heard.remove(route_key);
        }
    }

    /// **Say that `route_key`'s trade link was lost, and why** (`Some`: the link thread's `TradeLinkSignal::link_lost`) or
    /// that it is back (`None`). A second word on one outage (the close that follows a loss) keeps the first instant.
    pub fn set_trade_link_lost(&self, route_key: &str, why: Option<&str>) {
        let mut lost = self.trade_links.lock().unwrap_or_else(PoisonError::into_inner);
        match why {
            Some(why) => {
                let why = frame_words(why);
                lost.entry(route_key.to_string())
                    .and_modify(|(_, said)| said.clone_from(&why))
                    .or_insert_with(|| (Instant::now(), why));
            }
            None => {
                lost.remove(route_key);
            }
        }
    }

    /// **Say why `route_key`'s start was refused** (`Some`: the refusal's words) or that this start is a new one
    /// (`None`), so a cause outlives no later start.
    pub fn set_start_refusal(&self, route_key: &str, why: Option<&str>) {
        let mut refusals = self.start_refusals.lock().unwrap_or_else(PoisonError::into_inner);
        match why {
            Some(why) => {
                refusals.insert(route_key.to_string(), frame_words(why));
            }
            None => {
                refusals.remove(route_key);
            }
        }
    }

    /// Hand the registry the price feeds' dials, for the observe frame ([`LinkHealth::price_link_down`]).
    pub fn bind_price_links(&self, links: Arc<PriceLinks>) {
        let _ = self.price_links.set(links);
    }

    /// **Say what `route_key`'s balance gate holds back** (`balance_gate`): `Some(reason)` while the venue's cash is
    /// not handed to the engine, `None` once it is (or the engine is gone). The words are the operator's, shown as
    /// given by [`AccountReadiness::balance_held`].
    pub fn set_balance_held(&self, route_key: &str, reason: Option<&str>) {
        let mut held = self.held_balances.lock().unwrap_or_else(PoisonError::into_inner);
        match reason {
            Some(reason) => {
                held.insert(route_key.to_string(), reason.to_string());
            }
            None => {
                held.remove(route_key);
            }
        }
    }

    /// Record the currency the venue stated for `route_key`'s account at its adapter's start (`None`: it stated
    /// none, or the daemon predates the field, and a symbol quoted in another currency is then refused). Called by
    /// `VenueBackendGateway::start_adapter` beside [`Self::register`], so a later start of the account replaces it.
    pub fn anchor_currency(&self, route_key: &str, currency: Option<&str>) {
        self.unconfirmed.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.unanchorable.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        // A new start decides its own mount afresh ([`Self::note_foreign_mount`] follows when it books one).
        self.forget_foreign_mount(route_key);
        // A new start decides its own hold afresh (its gate writes it): a row of the engine it replaces is void.
        self.set_balance_held(route_key, None);
        self.assumed_holds.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        let mut anchored = self.anchored.lock().unwrap_or_else(PoisonError::into_inner);
        match currency.and_then(vike_model::currency_code) {
            Some(currency_value) => {
                anchored.insert(route_key.to_string(), currency_value);
            }
            None => {
                anchored.remove(route_key);
            }
        }
    }

    /// Say WHY `route_key`'s currency was not anchored (the engine's mount fence, `unanchorable_mount`): a call
    /// after [`Self::anchor_currency`]`(route_key, None)` of the same start.
    pub fn note_unanchorable_mount(&self, route_key: &str, why: String) {
        self.unanchorable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(route_key.to_string(), why);
    }

    /// [`Self::anchor_currency`] for a currency the VENUE did not state: the one the journal's ledger was anchored to,
    /// used because the read missed at this start (`anchor_with_journal`). (The assumed USD default is NOT this: it
    /// is flagged and warned, and trades inside the conversion's own gates, `anchor_is_unconfirmed`.) Nothing
    /// confirms the journal's, so a symbol quoted
    /// in another currency is refused on it until the account is started again and its own start states one (that
    /// start's `anchor_currency` clears this): a rate would otherwise be booked in a currency nobody checked, and
    /// the owner may have re-pointed the account since the journal was written.
    pub fn anchor_unconfirmed(&self, route_key: &str, currency: Option<&str>) {
        self.anchor_currency(route_key, currency);
        if currency.is_some_and(|c| !c.trim().is_empty()) {
            self.unconfirmed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(route_key.to_string());
        }
    }

    /// Record the `(symbol, quote currency)` pairs the journal says `route_key`'s restored engine held quoted in
    /// another currency. Called by `VenueBackendGateway::start_adapter` from the boot hints. An empty list forgets the
    /// account's entry; a stop of the engine forgets it too ([`Self::engine_stopped`]).
    pub fn note_journaled_foreign(&self, route_key: &str, symbols: &[(String, String)]) {
        let mut journaled = self.journaled.lock().unwrap_or_else(PoisonError::into_inner);
        if symbols.is_empty() {
            journaled.remove(route_key);
        } else {
            journaled.insert(route_key.to_string(), symbols.to_vec());
        }
    }

    /// Bind the answer to "how old is the engine's standing rate for this currency of this account?", from the
    /// core's published snapshot (once, by the root). Unbound, the timer cannot say a rate is not being refreshed
    /// ([`Self::unrefreshed_held`] answers nothing).
    pub fn bind_rate_rows(&self, rows: impl Fn(&str, &str) -> Option<i64> + Send + Sync + 'static) {
        let _ = self.rate_rows.set(Box::new(rows));
    }

    /// Bind each venue's feed handle (once, after the feeds are wired). Until it is, a price ask is
    /// answered `Err`: nothing can deliver one.
    pub fn bind_prices(&self, watchers: HashMap<String, StrategyLaneCexWatcher>) {
        let _ = self.prices.set(watchers);
    }

    /// Whether this node ADMITTED `symbol` into `route_key`'s engine but the account's current
    /// daemon was not told (`TradeLease::has_told`): the daemon behind the lease was replaced, or a
    /// re-teaching it refused. A symbol only looked at (never admitted) is not owed anything. `false`
    /// when the account has no lease: the lookup then names that.
    fn daemon_missed(&self, route_key: &str, symbol: &str) -> bool {
        let admitted = self
            .admitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(route_key.to_string(), symbol.to_string()));
        admitted
            && self
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(route_key)
                .is_some_and(|session| !session.has_told(symbol))
    }

    /// Bind the answer to "does this account run with an account-wide exposure ceiling?" for the
    /// engines the BOOT built, from the policy row and the account rows it read. Unbound, no account
    /// counts as capped (a live account is then refused a new symbol). An account started later in the
    /// running node is answered by [`Self::engine_starting`], not by this.
    pub fn bind_exposure_cap(&self, armed: impl Fn(&str) -> bool + Send + Sync + 'static) {
        let _ = self.exposure.set(Box::new(armed));
    }

    /// **The account watcher's word on the lane twins**, replacing the last: `real` are the route keys of
    /// operators' own accounts that spell a twin's label ([`AccountReadiness::real_account_under_twin_label`]), and
    /// `closing` the twins kept up only to be closed ([`AccountReadiness::lane_twin_closing_only`]). Called when
    /// either changed; the door reads both per command.
    pub fn set_lane_twin_flags(&self, real: &[String], closing: &[String]) {
        let mut flags = self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner);
        flags.real = real.iter().cloned().collect();
        flags.closing = closing.iter().cloned().collect();
    }

    /// **The route keys of the lane twins the node DERIVED from the account rows and runs**, replacing the last
    /// ([`AccountReadiness::derived_lane_twin`]). The watcher's memory of what the rows derived while the engine ran:
    /// a `live` engine is the door's twin only when it is in here.
    pub fn set_derived_lane_twins(&self, derived: &[String]) {
        self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner).derived =
            derived.iter().cloned().collect();
    }

    /// **The accounts the rows configure, by venue, replacing the last** ([`AccountReadiness::configured_accounts`]), or
    /// why the rows could not be read, which the door answers by refusing a bare venue.
    pub fn set_configured_accounts(
        &self,
        configured: Result<BTreeMap<String, Vec<String>>, String>,
    ) {
        self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner).configured =
            Some(configured);
    }

    /// **The account's engine is about to be (re)built by a hot start** (decision 0169), called BEFORE the
    /// start so the engine is never visible to the door under the previous engine's answer:
    /// `cap_armed` is whether the engine the start builds carries an account-wide ceiling, read from
    /// the rows the start will build it from ([`rows_arm_a_cap`]). The door's
    /// [`AccountReadiness::exposure_cap_armed`] answers this instead of the boot's rows, and the account's
    /// generation moves ([`Self::generation`]). Returns what the answer was, for
    /// [`Self::engine_start_refused`].
    pub fn engine_starting(&self, route_key: &str, cap_armed: bool) -> Option<bool> {
        let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        let state = engines.entry(route_key.to_string()).or_default();
        state.generation += 1;
        state.stopped = false;
        state.cap.replace(cap_armed)
    }

    /// **The account's engine was just started (in the running node, or by the boot) and its venue balance has
    /// not landed** (decisions 0169 and 0246): its equity is the seed cash until the reconcile driver's first
    /// pass over its leg, so [`AccountReadiness::awaiting_balance`] answers `Some`, with the cell's reason, and the
    /// door refuses its opening orders until `synced` has landed. Called by the start BEFORE the engine joins the
    /// core (a hot start's hook) or before the driver takes the boot's legs. A later start of the same account
    /// replaces the entry.
    pub fn engine_awaiting_balance(&self, route_key: &str, synced: &Arc<ReconcileFirstBalance>) {
        let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        engines.entry(route_key.to_string()).or_default().awaiting =
            Some((Arc::clone(synced), Instant::now()));
    }

    /// The start announced by [`Self::engine_starting`] built no engine: the cap answer goes back to
    /// `before` (the generation stays moved, which only ends work tied to the older one).
    pub fn engine_start_refused(&self, route_key: &str, before: Option<bool>) {
        let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        let state = engines.entry(route_key.to_string()).or_default();
        state.cap = before;
        // The start built no engine: there is still none, whatever [`Self::engine_starting`] said.
        state.stopped = true;
    }

    /// **Open the price lane of `symbol` for `route_key` now, ahead of any order**: what the door's first
    /// opening order on the symbol would do ([`SymbolAdmitter::price`] with `subscribe`), done at the engine's
    /// start instead. A lane twin's own symbol is mounted on no strategy, so nothing else prices it, and the
    /// first order after a hot start used to pay the facts look-up, the datahub dial and the pump's first
    /// frame while the door held it. Idempotent (an open pump is left alone), best effort (a failure is said
    /// and the order's own ask still stands), and BLOCKING (a lease round trip and a datahub subscribe, no
    /// lock held): run it off the account watcher's thread. The pump it opened is handed back to the idle
    /// reaper when the twin's engine stops ([`Self::engine_stopped`]), unless another twin or an admission holds it.
    /// While the twin runs the pump is held: another account's refused order on the same symbol
    /// ([`SymbolAdmitter::release`]) cannot idle it ([`Self::warmed_on_venue`]).
    pub fn warm_price(&self, route_key: &str, venue: &str, symbol: &str) {
        // The engine this warm is FOR: a stop (or a restart) during the seconds it takes ends it.
        let generation = self.generation(route_key);
        match SymbolAdmitter::price(self, route_key, venue, symbol, true) {
            Ok(_) if !self.keep_warmed(route_key, venue, symbol, generation) => tracing::info!(
                %route_key, %symbol,
                "the lane twin's engine stopped while its price lane was opening; the lane is handed \
                 back to the idle reaper"
            ),
            Ok(Some(_)) => tracing::info!(
                %route_key, %symbol,
                "the price lane of the lane twin's own symbol is open ahead of its first order"
            ),
            // No price yet, or the reaper was closing the pump and nothing was opened: the order's own ask
            // subscribes (again) and is held for the first price.
            Ok(None) => tracing::info!(
                %route_key, %symbol,
                "the price lane of the lane twin's own symbol was asked for ahead of its first order and \
                 has no price yet; the first order asks again and is held for it"
            ),
            Err(why) => tracing::warn!(
                %route_key, %symbol,
                "the price lane of the lane twin's own symbol could not be opened at its start ({why}); \
                 the first order on it asks again and is held for the first price"
            ),
        }
    }

    /// Remember that `route_key`'s twin warmed `symbol`'s pump, unless its engine stopped or restarted since
    /// `generation` (the stop already ran, and nothing would ever hand this pump back): then it is handed back
    /// here, and `false` is returned. The generation is checked under the lock [`Self::release_warmed`] takes,
    /// so a stop is either seen by this check or finds the entry this inserts.
    fn keep_warmed(&self, route_key: &str, venue: &str, symbol: &str, generation: u64) -> bool {
        {
            let mut warmed = self.warmed.lock().unwrap_or_else(PoisonError::into_inner);
            if self.generation(route_key) == generation {
                warmed.insert((route_key.to_string(), symbol.to_string()));
                // A release that ran before this entry (a sibling twin's stop) must not leave the lane to the reaper.
                if let Some(sub) = self
                    .watched
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_mut(&(venue.to_string(), symbol.to_string()))
                    && matches!(sub, Sub::Idle(_))
                {
                    *sub = Sub::Open;
                }
                return true;
            }
        }
        self.release_warmed(route_key, venue, symbol);
        false
    }

    /// Whether a running lane twin of `venue` warmed `symbol`'s pump ([`Self::keep_warmed`]) and has not stopped.
    fn warmed_on_venue(&self, venue: &str, symbol: &str) -> bool {
        self.warmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(key, s)| s == symbol && venue_of_route_key(key) == venue)
    }

    /// Hand back the pump `route_key`'s twin warmed for `symbol`: forget the entry, and give an open pump nobody
    /// else holds (another twin that warmed it, an account that admitted the symbol) to the idle reaper.
    fn release_warmed(&self, route_key: &str, venue: &str, symbol: &str) {
        let mut warmed = self.warmed.lock().unwrap_or_else(PoisonError::into_inner);
        warmed.remove(&(route_key.to_string(), symbol.to_string()));
        let held_by_another =
            warmed.iter().any(|(other, s)| s == symbol && venue_of_route_key(other) == venue);
        if held_by_another || self.admitted_on_venue(venue, symbol) {
            return;
        }
        if let Some(sub) = self
            .watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&(venue.to_string(), symbol.to_string()))
            && *sub == Sub::Open
        {
            *sub = Sub::Idle(Instant::now());
        }
    }

    /// The price watcher this node holds for `venue`, or the door's words for its absence.
    fn watcher(&self, venue: &str) -> Result<&StrategyLaneCexWatcher, String> {
        self.prices
            .get()
            .and_then(|watchers| watchers.get(venue))
            .ok_or_else(|| format!("this node runs no price feed for {venue}"))
    }

    /// The account's ENGINE GENERATION: moves at every hot start and stop of its engine. Work tied to an
    /// engine (the boot's retry of the symbols it missed) captures it and ends when it moves: the new
    /// engine's own start does its own admission, through the door's gates.
    pub fn generation(&self, route_key: &str) -> u64 {
        self.engines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .map_or(0, |state| state.generation)
    }

    /// **The account's engine was stopped** (decision 0169): the symbols this node admitted into it die
    /// with it (the next engine under the key starts with none). Forgetting them lets
    /// [`SymbolAdmitter::release`] and [`SymbolAdmitter::price`] treat a symbol the new engine asks for as
    /// not-yet-admitted again, and hands a symbol no other account of the venue admitted to the idle
    /// reaper, so its pump is not permanent.
    ///
    /// The generation moves, the door's ceiling answer becomes "none" (a stopped account takes no new
    /// symbol, and a live one is not read as capped on the strength of a dead engine) until the next
    /// start says otherwise, and the account's lease session is forgotten, so nothing is asked of a
    /// dead lease.
    pub fn engine_stopped(&self, route_key: &str) {
        {
            let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
            let state = engines.entry(route_key.to_string()).or_default();
            state.generation += 1;
            state.cap = Some(false);
            // A stopped account takes no order at all; the next start announces its own wait.
            state.awaiting = None;
            state.stopped = true;
        }
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        // Its combo legs go with it: the next engine under the key holds none, and its lease was told nothing.
        self.combo_legs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(rk, _), _| rk != route_key);
        self.own_grids.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        let gone: Vec<String> = {
            let mut admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
            let gone = admitted
                .iter()
                .filter(|(key, _)| key == route_key)
                .map(|(_, symbol)| symbol.clone())
                .collect();
            admitted.retain(|(key, _)| key != route_key);
            gone
        };
        self.anchored.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.unconfirmed.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.unanchorable.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.forget_foreign_mount(route_key);
        self.set_balance_held(route_key, None);
        self.assumed_holds.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.set_venue_link_down(route_key, None);
        self.note_venue_path_up(route_key, false);
        self.set_trade_link_lost(route_key, None);
        self.journaled.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        // A rate kept for the old engine's account is judged again by the next start (its currency, its ceiling).
        self.answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _), (_, a)| key != route_key || a.quote_rate.is_none());
        // The next engine under the key starts with no per-symbol margin rate, so no figure outlives this one.
        self.leverage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _), _| key != route_key);
        self.idle_admitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _), _| key != route_key);
        // A give-back asked of the old engine dies with it: the next engine under the key starts with no symbol.
        self.releasing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _), _| key != route_key);
        let venue = venue_of_route_key(route_key);
        for symbol in gone {
            if self.admitted_on_venue(venue, &symbol) {
                continue;
            }
            // Nobody admits it now: the pump (open, or closed by the admitted-pair reaper) is the idle reaper's.
            self.reaped
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&(venue.to_string(), symbol.clone()));
            self.watched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry((venue.to_string(), symbol))
                .or_insert_with(|| Sub::Idle(Instant::now()));
        }
        // The pump a lane twin WARMED is in no `admitted` set (its symbol is the engine's own, admitted by
        // nobody): hand it back too, unless another twin or an account's admission holds the symbol.
        let warmed_here: Vec<String> = self
            .warmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(key, _)| key == route_key)
            .map(|(_, symbol)| symbol.clone())
            .collect();
        for symbol in warmed_here {
            self.release_warmed(route_key, venue, &symbol);
        }
        // …and the lane its OWN symbol was held on ([`Self::hold_price`]): a stopped engine is priced by nobody.
        let held_here: Vec<String> = self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .filter(|(key, _)| key == route_key)
            .map(|(_, symbol)| symbol.clone())
            .collect();
        for symbol in held_here {
            self.drop_hold(route_key, venue, &symbol);
        }
    }

    /// Shorten [`IDLE_TTL`] (once; for a test that must not wait half a minute).
    pub fn set_idle_ttl(&self, ttl: Duration) {
        let _ = self.idle_ttl.set(ttl);
    }

    /// How long the pump of a released symbol stays open ([`IDLE_TTL`] unless a test says otherwise).
    fn idle_ttl(&self) -> Duration {
        self.idle_ttl.get().copied().unwrap_or(IDLE_TTL)
    }

    /// Whether ANY account of `venue` admitted `symbol`: its pump is then the engine's and permanent,
    /// whichever account's refused order left a watched entry beside it.
    fn admitted_on_venue(&self, venue: &str, symbol: &str) -> bool {
        self.admitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(route_key, s)| s == symbol && venue_of_route_key(route_key) == venue)
    }

    /// Take (or find) the place of `key`'s pump under [`MAX_WATCHED`]. A full set first gives its oldest
    /// idle pump up; `Err` only when every place is held by a pump someone is using.
    fn claim_pump(&self, key: &(String, String)) -> Result<Claim, String> {
        for _ in 0..2 {
            {
                let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
                match watched.get(key).copied() {
                    // Asked again: the pump is still open, the symbol is in use once more.
                    Some(Sub::Idle(_)) => {
                        watched.insert(key.clone(), Sub::Open);
                        return Ok(Claim::Held);
                    }
                    Some(Sub::Opening | Sub::Open) => return Ok(Claim::Held),
                    Some(Sub::Closing) => return Ok(Claim::Closing),
                    None if watched.len() < MAX_WATCHED => {
                        watched.insert(key.clone(), Sub::Opening);
                        return Ok(Claim::Subscribe);
                    }
                    None => {}
                }
            }
            if self.close_idle(Duration::ZERO, 1) == 0 {
                break;
            }
        }
        Err(format!(
            "this node already holds {MAX_WATCHED} price subscriptions for symbols it has not \
             admitted; retry once they clear"
        ))
    }

    /// Close the pumps of symbols the door let go of without admitting them and that have been idle for
    /// `older_than`, oldest first, at most `limit`. Returns how many places were freed. A symbol some
    /// account admitted meanwhile frees its place but keeps its pump. **A pump with a venue gap open is
    /// closed like any other, and its gap is ended with it** ([`crate::feeds::StrategyLaneCexWatcher::close_ticks`] says
    /// `Live` for the symbol, so the core's link dead-man window for it closes instead of tripping after its
    /// grace: a cancel and a HALT of the venue for a symbol no account admitted). It used to be kept
    /// while the gap stayed open, on every venue the dead-man arms, and a pair whose stream cannot connect
    /// held its place for ever. The datahub round trips run with no lock held.
    fn close_idle(&self, older_than: Duration, limit: usize) -> usize {
        let Some(watchers) = self.prices.get() else { return 0 };
        let picked: Vec<(Instant, (String, String))> = {
            let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
            let mut idle: Vec<_> = watched
                .iter()
                .filter_map(|(key, sub)| match sub {
                    Sub::Idle(since) if since.elapsed() >= older_than => {
                        Some((*since, key.clone()))
                    }
                    _ => None,
                })
                .collect();
            idle.sort();
            idle.truncate(limit);
            for (_, key) in &idle {
                watched.insert(key.clone(), Sub::Closing);
            }
            idle
        };
        let mut freed = 0;
        for (_, key) in picked {
            let (venue, symbol) = &key;
            let watcher = watchers.get(venue);
            let keep_pump =
                self.admitted_on_venue(venue, symbol) || self.held_on_venue(venue, symbol);
            // Read before the close, which ends the gap.
            let under_gap = !keep_pump && watcher.is_some_and(|w| w.gap_open(venue, symbol));
            let closed = !keep_pump && watcher.is_some_and(|w| w.close_ticks(venue, symbol));
            let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
            // Only the entry this call marked: `admitted` may have removed it meanwhile.
            if watched.get(&key) != Some(&Sub::Closing) {
                continue;
            }
            watched.remove(&key);
            freed += 1;
            if closed {
                tracing::info!(
                    %venue, %symbol, under_gap,
                    "closed the price subscription of a symbol that was never admitted{}",
                    if under_gap {
                        " (its venue gap was open: ended with it, so the link dead-man's window for it is closed)"
                    } else {
                        ""
                    }
                );
            }
        }
        freed
    }

    /// Close every pump that has been idle for [`IDLE_TTL`] ([`Self::close_idle`]).
    pub fn close_idle_pumps(&self) -> usize {
        self.close_idle(self.idle_ttl(), usize::MAX)
    }

    /// Start the thread that calls [`Self::close_idle_pumps`] every [`REAP_EVERY`] until the registry is
    /// dropped (it holds only a `Weak`).
    pub fn spawn_idle_reaper(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-reaper".into()).spawn(move || {
                loop {
                    std::thread::sleep(REAP_EVERY);
                    let Some(facts) = weak.upgrade() else { return };
                    facts.close_idle_pumps();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the idle price-subscription reaper ({e}); idle pumps close only \
                 when a full set needs the place"
            );
        }
    }

    /// Tell the daemon behind `route_key`'s lease that `symbol` is admitted
    /// (`TradeLease::admit_symbol_confirmed`), one round trip with no lock held, at the bound of the boot
    /// window or the thread's retry round. `None`: the account has no open lease.
    ///
    /// The commit is the one request that may ask the exchange anything: on a venue that posts a leverage the
    /// daemon makes it hold the account's figure for the symbol and reads it back, and the reply names the
    /// figure it SAW. That figure is kept for [`SymbolAdmitter::confirmed_leverage`] (set on `Some`, removed on
    /// `None`; a commit that fails says nothing and leaves what is kept).
    ///
    /// ⚠ **The commit takes seconds on a venue that posts a leverage** (three or four signed reads, at most one
    /// write, a poll to see it took), where it used to be one round trip. Two rules keep that off the paths
    /// that cannot afford it: a symbol the daemon was told and the node holds a figure for is NOT committed
    /// again ([`Self::settled`]: a repeat commit on an actively traded symbol is the one that could move the
    /// leverage under an open position); and a commit that times out or fails is "unconfirmed" and nothing
    /// else: the symbol stays at the engine's own rate, and the daemon, which keeps going, is read back by the
    /// next commit that runs.
    fn commit_to_daemon(&self, route_key: &str, symbol: &str) -> Option<Result<(), String>> {
        self.commit(route_key, symbol).map(|told| told.map_err(|e| e.text().to_string()))
    }

    /// [`Self::commit_to_daemon`], saying whether a failure may heal ([`SymbolPropertiesRequestError::Unavailable`]) or is the
    /// daemon's answer about the symbol ([`SymbolPropertiesRequestError::Refused`]).
    fn commit(
        &self,
        route_key: &str,
        symbol: &str,
    ) -> Option<Result<(), SymbolPropertiesRequestError>> {
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned()?;
        // The engine this commit is FOR: a stop (and a start) during the seconds it takes ends it unheard.
        let generation = self.generation(route_key);
        let told = self.ask(route_key, |bound| match bound {
            Some(bound) => session.admit_symbol_confirmed_within(symbol, bound),
            None => session.admit_symbol_confirmed(symbol),
        });
        Some(match told {
            Ok(_) if self.generation(route_key) != generation => {
                tracing::debug!(
                    %route_key, %symbol,
                    "the account's engine changed while the commit ran; its figure is not kept"
                );
                Ok(())
            }
            Ok(figure) => {
                self.keep_leverage(route_key, symbol, figure);
                Ok(())
            }
            Err(e) => Err(e),
        })
    }

    /// Whether a commit for `symbol` has nothing left to do: the account's CURRENT daemon was told the symbol
    /// (`TradeLease::has_told`) and the venue was SEEN holding a leverage for it ([`Self::leverage`]). The
    /// figure is the account's own and fixed for the engine's life (it is read when the engine starts, and a
    /// stop forgets it), so asking again could only find the same answer, or, if the venue's position has
    /// moved meanwhile, move the leverage under it. Residual: a leverage changed at the exchange by hand is
    /// not seen until the engine restarts.
    fn settled(&self, route_key: &str, symbol: &str) -> bool {
        self.leverage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&(route_key.to_string(), symbol.to_string()))
            && self
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(route_key)
                .is_some_and(|session| session.has_told(symbol))
    }

    /// **Tell the daemon behind `route_key`'s lease the LEGS of a combo its engine holds**, and keep them in
    /// [`Self::combo_legs`] for [`Self::settle_combo_legs`]: the commit of an admission and nothing else. The core wrote
    /// the legs into the engine itself and sent no `AdmitSymbol`, so without this the daemon's reconcile client never
    /// read a leg: a held leg was an `OrphanLocalPosition` on every pass once the bridge's own memory of the leg's fills
    /// was gone (a gatewayhub restart). Told, the leg is in the lease's record (`TradeLease::has_told`) and the
    /// re-teaching worker tells a restarted gatewayhub again. Not an admission of this registry: no place, no price lane.
    ///
    /// `combos` are the order client ids of the combos that name the legs (the core's hook gives one; the boot the live
    /// ones the journal restored): while one of them is live the legs stay read. Idempotent: a leg the current daemon
    /// was told costs no request, and an account with no lease (a `paper` engine) asks nothing and keeps nothing. A leg
    /// the gatewayhub could not be asked is OWED, asked again on the reaper's ticks; one the daemon REFUSES (deribit's
    /// coin-settled option, whose facts are refused) is one warning and never asked again, and the bridge then reads it
    /// only as its own fills teach it.
    pub fn tell_combo_legs(&self, route_key: &str, combos: &[String], legs: &[String]) {
        if !self.sessions.lock().unwrap_or_else(PoisonError::into_inner).contains_key(route_key) {
            return;
        }
        let now = Instant::now();
        for leg in legs {
            {
                let mut kept = self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner);
                let entry =
                    kept.entry((route_key.to_string(), leg.clone())).or_insert_with(|| ComboLeg {
                        combos: Vec::new(),
                        named: now,
                        told: false,
                        owed: None,
                        forgetting: None,
                    });
                entry.named = now;
                entry.forgetting = None;
                for combo in combos {
                    if !entry.combos.contains(combo) {
                        entry.combos.push(combo.clone());
                    }
                }
            }
            self.commit_leg(route_key, leg);
        }
    }

    /// One leg's commit (none when the current daemon was told it already), and what it leaves in
    /// [`Self::combo_legs`]: told; owed since the first failure that may heal; or refused, and never asked again.
    fn commit_leg(&self, route_key: &str, leg: &str) {
        let told_already = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .is_some_and(|session| session.has_told(leg));
        let outcome = if told_already { Some(Ok(())) } else { self.commit(route_key, leg) };
        let mut kept = self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = kept.get_mut(&(route_key.to_string(), leg.to_string())) else { return };
        match outcome {
            // The lease went during the round trip: its engine's stop empties the table.
            None => {}
            Some(Ok(())) => {
                entry.told = true;
                entry.owed = None;
            }
            Some(Err(e)) if e.is_transient() => {
                let why = e.text();
                if entry.owed.is_none() {
                    entry.owed = Some(Instant::now());
                    tracing::warn!(
                        %route_key, symbol = %leg,
                        "the gatewayhub could not be told the combo leg `{leg}` ({why}); asked again on the next \
                         ticks for up to {COMBO_LEG_RETRY_FOR:?}"
                    );
                }
            }
            Some(Err(e)) => {
                let why = e.text();
                entry.owed = None;
                tracing::warn!(
                    %route_key, symbol = %leg,
                    "the gatewayhub refused the combo leg `{leg}` ({why}); its reconcile client reads the leg only \
                     from fills it saw itself"
                );
            }
        }
    }

    /// **One tick of the combo legs' keeper**, on the admitted-pair reaper's thread ([`Self::spawn_admitted_reaper`],
    /// never the fold), for each leg [`Self::combo_legs`] keeps, read off the published `holdings`:
    ///
    /// - **Needed** (its engine holds a non-flat position in it, a combo that named it is live, or a combo named it
    ///   within [`COMBO_LEG_QUIET`]): an OWED commit is asked again, until [`COMBO_LEG_RETRY_FOR`] has passed since its
    ///   first failure, then given up at `warn`.
    /// - **Needed by nothing**: the daemon is told to stop reading it (`TradeLease::forget_symbol`), so it costs no
    ///   read and its place in the daemon's list and the lease's record is free; a gatewayhub that did not answer is
    ///   asked again until [`COMBO_LEG_RETRY_FOR`]. A leg never told is only dropped, and a leg the door ADMITTED too is
    ///   the admission's to give back: dropped here, never forgotten. ⚠ The ENGINE keeps listing the leg (to
    ///   `vike_execution::Engine::unadmit_symbol` a combo leg is configuration, not a slot), so its own place under
    ///   `MAX_EXTRA_SYMBOLS` stays taken.
    /// - **Its engine published nothing yet**: left for the next tick.
    ///
    /// A leg a later combo names is told again ([`Self::tell_combo_legs`]). BLOCKING: the round trips run with no lock.
    pub fn settle_combo_legs(&self, holdings: &crate::readmit::ReadmitHoldings) {
        let legs: Vec<((String, String), ComboLeg)> = self
            .combo_legs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, leg)| (key.clone(), leg.clone()))
            .collect();
        for (key, leg) in legs {
            let (route_key, symbol) = (&key.0, &key.1);
            if !holdings.known.contains(route_key) {
                continue;
            }
            let needed = leg.named.elapsed() < COMBO_LEG_QUIET
                || leg.combos.iter().any(|combo| holdings.live_orders.contains(combo))
                || holdings.held.iter().any(|t| t.route_key == *route_key && t.symbol == *symbol);
            if needed {
                match leg.owed {
                    Some(since) if since.elapsed() >= COMBO_LEG_RETRY_FOR => {
                        if let Some(entry) = self
                            .combo_legs
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get_mut(&key)
                        {
                            entry.owed = None;
                        }
                        tracing::warn!(
                            %route_key, %symbol,
                            "the gatewayhub was not told the combo leg `{symbol}` in {COMBO_LEG_RETRY_FOR:?}; its \
                             reconcile client reads the leg only from fills it saw itself"
                        );
                    }
                    Some(_) => self.commit_leg(route_key, symbol),
                    None => {}
                }
                continue;
            }
            let admitted =
                self.admitted.lock().unwrap_or_else(PoisonError::into_inner).contains(&key);
            if !leg.told || admitted {
                self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
                continue;
            }
            self.forget_leg(&key, &leg);
        }
    }

    /// Tell the daemon to stop reading a combo leg nothing needs ([`Self::settle_combo_legs`]). A combo that named the
    /// leg during the round trip wins: the leg is committed again, so the daemon's FIFO ends on its commit.
    fn forget_leg(&self, key: &(String, String), leg: &ComboLeg) {
        let (route_key, symbol) = (&key.0, &key.1);
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned();
        let Some(session) = session else {
            self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner).remove(key);
            return;
        };
        let answer = self.ask(route_key, |bound| match bound {
            Some(bound) => session.forget_symbol_within(symbol, bound),
            None => session.forget_symbol(symbol),
        });
        let mut kept = self.combo_legs.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.get(key).is_some_and(|now| now.named != leg.named) {
            drop(kept);
            self.commit_leg(route_key, symbol);
            return;
        }
        match answer {
            Ok(()) => {
                kept.remove(key);
                tracing::info!(
                    %route_key, %symbol,
                    "no combo and no position needs the combo leg `{symbol}` any more: the gatewayhub's reconcile \
                     client stops reading it and its place is free"
                );
            }
            Err(e) if !e.is_transient() => {
                let why = e.text();
                kept.remove(key);
                tracing::warn!(
                    %route_key, %symbol,
                    "the gatewayhub did not forget the combo leg `{symbol}` ({why}); its reconcile client keeps \
                     reading it"
                );
            }
            Err(e) => {
                let why = e.text();
                let since = leg.forgetting.unwrap_or_else(Instant::now);
                if since.elapsed() >= COMBO_LEG_RETRY_FOR {
                    kept.remove(key);
                    tracing::warn!(
                        %route_key, %symbol,
                        "the gatewayhub was not told to stop reading the combo leg `{symbol}` ({why}) in \
                         {COMBO_LEG_RETRY_FOR:?}; its reconcile client keeps reading it until the account's engine \
                         stops"
                    );
                } else if let Some(entry) = kept.get_mut(key) {
                    entry.forgetting = Some(since);
                    tracing::debug!(%route_key, %symbol, "the combo leg's forget is not answered yet ({why})");
                }
            }
        }
    }

    /// **The core's `vike_core::CoreConfig::on_combo_legs` over this registry**: each accepted combo's legs go down a
    /// channel to one `combo-legs` thread that runs [`Self::tell_combo_legs`], so the round trips never run on the
    /// fold thread. The thread ends with the core (the closure holds the sender) or with this registry.
    pub fn combo_leg_teller(self: &Arc<Self>) -> vike_core::ComboLegsHook {
        let (tx, rx) = std::sync::mpsc::channel::<(String, String, Vec<String>)>();
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new().name("combo-legs".into()).spawn(move || {
            while let Ok((route_key, combo, legs)) = rx.recv() {
                let Some(facts) = weak.upgrade() else { return };
                facts.tell_combo_legs(&route_key, std::slice::from_ref(&combo), &legs);
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the combo-leg thread ({e}); a combo's legs are read by the gatewayhub's \
                 reconcile client only from fills it saw itself"
            );
        }
        Box::new(move |route_key: &str, combo: &str, legs: &[String]| {
            let _ = tx.send((route_key.to_string(), combo.to_string(), legs.to_vec()));
        })
    }

    /// Keep (`Some`) or forget (`None`) the leverage a commit confirmed. A figure that is no leverage (below
    /// 1x or not finite: the engine would refuse the whole admission it rides) is no figure.
    ///
    /// `None` after a `Some` removes the figure and says so: the engine cannot be told to forget the row it
    /// holds (a repeat `None` leaves the old per-symbol row), so the symbol stays at the rate last stated, and
    /// the node simply stops restating a figure the venue was not seen holding this time.
    fn keep_leverage(&self, route_key: &str, symbol: &str, figure: Option<f64>) {
        let key = (route_key.to_string(), symbol.to_string());
        let mut kept = self.leverage.lock().unwrap_or_else(PoisonError::into_inner);
        match figure.filter(|x| x.is_finite() && *x >= 1.0) {
            Some(x) => {
                kept.insert(key, x);
            }
            None => {
                if let Some(was) = kept.remove(&key) {
                    tracing::warn!(
                        %route_key, %symbol,
                        "the venue confirmed no leverage this time (it was seen at {was}x); the engine \
                         keeps the margin rate it has for the symbol"
                    );
                }
            }
        }
    }

    /// `admission` stating the symbol's own margin rate (`1 / leverage`) when a commit confirmed a leverage for
    /// it, else the rate the VENUE states for the symbol (`admission.initial_margin_requirement` as [`Self::lookup`] left it:
    /// IG's `marginFactor`, the facts' `margin_rate`; `None` for every venue that states none), and none
    /// otherwise: what a facts answer carries out, so a refresh restates the rate. A leverage the venue was SEEN
    /// holding is the account's own choice and wins over the venue's standing figure.
    fn with_leverage(&self, mut admission: SymbolAdmission) -> SymbolAdmission {
        let confirmed = self
            .leverage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(admission.route_key.clone(), admission.symbol.clone()))
            .map(|x| 1.0 / x);
        admission.initial_margin_requirement = confirmed.or(admission.initial_margin_requirement);
        admission
    }

    /// The facts of `symbol` over `route_key`'s lease, ignoring the cache.
    fn lookup(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned();
        let Some(session) = session else {
            // An account with no trade lease (a `paper` one: it never logs in) is answered by the keyless look-up the
            // root bound (the owner, 2026-11-16: a polymarket paper account's token, like every other venue's
            // symbol, fills at the current quote).
            if let Some(facts) =
                self.keyless.get().and_then(|keyless| keyless(route_key, venue, symbol))
            {
                return self.keyless_admission(facts, route_key, venue, symbol);
            }
            return Err(SymbolPropertiesRequestError::Unavailable(format!(
                "`{route_key}` has no open trade lease at the gatewayhub on this node, so \
                 {venue} cannot be asked about `{symbol}`"
            )));
        };
        let facts = self.ask(route_key, |bound| match bound {
            Some(bound) => session.symbol_facts_within(symbol, bound),
            None => session.symbol_facts(symbol),
        })?;
        // The venue may have just NAMED the currency the engine only assumed: that ends the hold on its cash.
        self.confirm_assumed_anchor(route_key, venue, &facts);
        // THE SYMBOL'S CURRENCY (decision 0183, phase 1). An answer that names none is a symbol quoted in
        // the account's own currency (the daemon's fence is what keeps that true); one that names another
        // is admitted only with a usable, fresh rate, and only for an account the rule lets convert: a
        // refusal here is the symbol NOT admitted, at the boot's re-admission and at the timer as well as
        // at the door, because every one of them asks through this function.
        let quote = match super::quoted::judge(venue, symbol, &facts, vike_model::now_ms()) {
            Ok(quote) => quote,
            Err(super::quoted::SymbolPropertiesVerdict::Refused(why)) => {
                return Err(SymbolPropertiesRequestError::Refused(why));
            }
            Err(super::quoted::SymbolPropertiesVerdict::Unavailable(why)) => {
                return Err(SymbolPropertiesRequestError::Unavailable(why));
            }
        };
        if quote.is_some()
            && let Some(why) = crate::server::symbol_admission::quote_conversion_refusal(
                venue,
                Some(session.account().tier),
                self.exposure_cap_armed(route_key),
            )
        {
            return Err(SymbolPropertiesRequestError::Refused(format!(
                "{venue}: `{symbol}` is quoted in {} and cannot be admitted: {why}",
                quote.as_ref().map_or("another currency", |q| q.currency.as_str())
            )));
        }
        if let Some(q) = &quote {
            let anchor = self
                .anchored
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(route_key)
                .cloned();
            let unconfirmed =
                self.unconfirmed.lock().unwrap_or_else(PoisonError::into_inner).contains(route_key);
            let Some(anchor) = anchor else {
                // The venue DID state a currency; the engine's mount fence declined to anchor the ledger to it.
                if let Some(why) =
                    self.unanchorable.lock().unwrap_or_else(PoisonError::into_inner).get(route_key)
                {
                    return Err(SymbolPropertiesRequestError::Refused(format!(
                        "{venue}: `{symbol}` is quoted in {} and is NOT admitted: this account's ledger is not \
                         anchored to its currency, so no rate can be tied to it: {why}",
                        q.currency
                    )));
                }
                return Err(SymbolPropertiesRequestError::Refused(format!(
                    "{venue}: `{symbol}` is quoted in {} and this account's own currency is not known (the \
                     venue's account summary was not read when the account started, or the gatewayhub \
                     predates it), so the rate cannot be tied to a currency the ledger is in. The symbol is \
                     NOT admitted: set the account's `account.active` off and on to start it again",
                    q.currency
                )));
            };
            // A gatewayhub that states the ledger currency its rate is expressed in (ibkr's, which reads the
            // account's currency apart from the start) is held to the one the engine was anchored to: the
            // anchor may have come from the journal when the start could not read it, and a currency that
            // changed meanwhile would put every rate in a unit the engine does not count in.
            match facts.account_currency.as_deref() {
                Some(stated) if !vike_model::same_currency(stated, &anchor) => {
                    return Err(SymbolPropertiesRequestError::Refused(format!(
                        "{venue}: `{symbol}`'s rate is expressed in {} and this account's engine is anchored \
                         to {anchor}, so the rate is NOT used and the symbol is NOT admitted: the account's \
                         currency changed since the engine was built, or the engine's {anchor} was only \
                         ASSUMED (the venue stated none at its start) and the venue states another one now; \
                         set the account's `account.active` off and on to start it again",
                        stated.trim()
                    )));
                }
                // A venue whose rate always states its ledger (its row's `rate_ledger_stamp` is `Required`: ibkr):
                // one that does not is an answer nobody checked against the anchor (an older or different
                // gatewayhub), and no rate is booked on a unit nobody confirmed. Likewise on an anchor taken from the
                // journal that no stated rate can confirm (`anchor_unconfirmed`): the account may have been
                // re-pointed to another currency since.
                None if unconfirmed
                    || vike_venues::venue_currency::venue_currency_for(venue).rate_ledger_stamp
                        == vike_venues::venue_currency::VenueRateLedgerStamp::Required =>
                {
                    let from = if unconfirmed {
                        ", taken from the journal because the venue did not state the account's currency \
                         when the account started"
                    } else {
                        ""
                    };
                    return Err(SymbolPropertiesRequestError::Refused(format!(
                        "{venue}: `{symbol}`'s rate does not say which currency it is expressed in, so it cannot \
                         be checked against the currency this account's engine is anchored to (the engine is \
                         anchored to {anchor}{from}): the rate is NOT used and the symbol is NOT admitted. \
                         Set the account's `account.active` off and on to start it again"
                    )));
                }
                _ => {}
            }
        }
        // A symbol quoted in the account's OWN money carries no rate, but a gatewayhub that STATES the ledger its
        // answer is in (oanda's, ctrader's, fxcm's and ibkr's) is held to the engine's anchor here too: the
        // notional and the P/L of such a symbol are in that ledger's money, and an engine anchored to another one
        // (an ASSUMED USD the start fell back on, while the venue states EUR now) would book them as its own.
        // An engine with no anchor has nothing to compare with, and an answer that states none is an older
        // gatewayhub's: both are admitted as before.
        if quote.is_none()
            && let Some(stated) = facts.account_currency.as_deref()
            && let Some(anchor) =
                self.anchored.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned()
            && !vike_model::same_currency(stated, &anchor)
        {
            return Err(SymbolPropertiesRequestError::Refused(format!(
                "{venue}: `{symbol}` is quoted in the account's own currency, but the gatewayhub's answer is \
                 expressed in {} and this account's engine is anchored to {anchor}, so the answer is NOT used \
                 and the symbol is NOT admitted: the account's currency changed since the engine was built, \
                 or the engine's {anchor} was only ASSUMED (the venue stated none at its start) and the venue \
                 states another one now; set the account's `account.active` off and on to start it again",
                stated.trim()
            )));
        }
        // The symbol the facts are FOR: the one asked, unless the daemon spelled it otherwise (polymarket: a market
        // NAME asked, the outcome's token id answered). The admission, the answer cache and everything the door
        // does next are under THAT spelling, so one position keeps one name.
        let symbol = facts.symbol.as_deref().unwrap_or(symbol);
        let admission = SymbolAdmission {
            route_key: route_key.to_string(),
            symbol: symbol.to_string(),
            grid: SymbolGrid::from_properties(&facts.properties),
            contract_value: facts.contract_value,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: quote.as_ref().map(|q| q.currency.clone()),
            quote_rate: quote.as_ref().map(|q| q.rate),
            rate_as_of_ms: quote.as_ref().map(|q| q.as_of_ms),
            // The rate the VENUE states for the symbol (IG's `marginFactor`), none for a venue that states none; a
            // figure a COMMIT confirmed (a look asks the exchange nothing) is laid over it just below.
            initial_margin_requirement: facts.margin_rate,
            release: false,
            // The margin mode the venue holds the symbol in, when it is not cross (an isolated-only coin, a
            // spot pair): a daemon sends it only to a lease that asked for it (`modes`, decision 0218).
            margin_mode: facts.margin_mode,
        };
        // The cache keeps the answer BEFORE the leverage is laid over it (the venue's own rate, if any), and every
        // reader lays the CURRENT figure over it ([`Self::with_leverage`]): a figure forgotten since is not
        // restated from an old answer.
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).insert(
            (route_key.to_string(), symbol.to_string()),
            (Instant::now(), admission.clone()),
        );
        Ok(self.with_leverage(admission))
    }
}

impl SymbolAdmitter for TradeLeaseRegistry {
    fn price_source(&self, venue: &str) -> Result<(), String> {
        self.watcher(venue).map(|_| ())
    }

    /// A name is resolved by ASKING THE FACTS over the lease: the gatewayhub resolves it (Gamma is a venue
    /// socket, and this node opens none) and answers the facts of the symbol it resolved to with that symbol's
    /// spelling (`SymbolPropertiesWire::symbol`). `Self::lookup` files the answer under the resolved symbol, so the
    /// planner's next ask for it (the admission) is a cache hit and costs no second round trip. A refusal is
    /// remembered like any other (30 s, under the typed text).
    fn resolve(&self, route_key: &str, venue: &str, typed: &str) -> Result<String, String> {
        self.admission(route_key, venue, typed)
            .map(|admission| admission.symbol)
            .map_err(|e| e.text().to_string())
    }

    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        let key = (route_key.to_string(), symbol.to_string());
        let cached = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            // An answer that carries a conversion rate is only as good as the rate ([`RATE_FRESH_FOR`]).
            .filter(|(at, a)| at.elapsed() < kept_for(a))
            .map(|(_, admission)| admission.clone());
        // A cached answer stands in for the daemon unless it is an ADMITTED symbol the daemon's reconcile client was not
        // told: after a gatewayhub restart (or a re-teaching it refused) nobody told
        // the new one, and the cache must not keep the request from reaching it.
        if let Some(admission) = cached
            && !self.daemon_missed(route_key, symbol)
        {
            // The kept answer may predate the last commit's figure.
            return Ok(self.with_leverage(admission));
        }
        if let Some((at, why)) =
            self.failed.lock().unwrap_or_else(PoisonError::into_inner).get(&key)
            && at.elapsed() < NEGATIVE_TTL
        {
            return Err(why.clone());
        }
        let looked = self.lookup(route_key, venue, symbol);
        let mut failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        match &looked {
            Ok(_) => {
                failed.remove(&key);
            }
            // Only a DEFINITIVE answer is remembered; a lost link, a timeout or a busy or restarting daemon
            // says nothing about the symbol, so the next order asks again.
            Err(e) if e.is_transient() => {
                failed.remove(&key);
            }
            Err(e) => {
                remember_refusal(&mut failed, key, e.clone());
            }
        }
        looked
    }

    /// The start's own figures for the mounted symbol, else a facts answer younger than [`FACTS_TTL`] (the rate an
    /// answer may carry ages faster, the grid does not). Two map reads: no lease is asked.
    fn held_grid(&self, route_key: &str, symbol: &str) -> Option<SymbolGrid> {
        let own = self.own_grids.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, grid)) = own.get(route_key).filter(|(mounted, _)| mounted == symbol) {
            return Some(grid.clone());
        }
        drop(own);
        self.answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(route_key.to_string(), symbol.to_string()))
            .filter(|(at, _)| at.elapsed() < FACTS_TTL)
            .map(|(_, admission)| admission.grid.clone())
    }

    fn price(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        let watcher = self.watcher(venue)?;
        if subscribe {
            // A committing ask is activity: the snapshot does not show the order yet, so the reaper starts counting
            // the symbol's idleness again ([`Self::reap_idle_admitted`]).
            self.idle_admitted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&(route_key.to_string(), symbol.to_string()));
            let key = (venue.to_string(), symbol.to_string());
            // An ADMITTED symbol is the engine's to bound; only the not-yet-admitted ones are counted
            // here, so a boot that subscribes the restored symbols after re-admitting them leaves no
            // entry behind.
            let counted = !self
                .admitted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&(route_key.to_string(), symbol.to_string()));
            let first = if counted {
                match self.claim_pump(&key)? {
                    Claim::Subscribe => true,
                    Claim::Held => false,
                    Claim::Closing => return Ok(None),
                }
            } else {
                true
            };
            if first {
                // The two RPCs run with NO lock held. A failure forgets the entry so the next ask
                // tries again; a second asker meanwhile reads the price and gets "none yet".
                let opened = self.lane_tick(route_key, venue, symbol).and_then(|tick| {
                    watcher.subscribe_ticks(symbol, tick).map_err(|e| {
                        format!("cannot subscribe {venue}'s prices for `{symbol}`: {e}")
                    })
                });
                if counted {
                    let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
                    match &opened {
                        // Only the entry this ask opened: an `admitted` that removed it meanwhile
                        // made the pump the admitted symbol's, which this map no longer counts.
                        Ok(()) => {
                            if let Some(sub) = watched.get_mut(&key)
                                && *sub == Sub::Opening
                            {
                                *sub = Sub::Open;
                            }
                        }
                        Err(_) => {
                            watched.remove(&key);
                        }
                    }
                }
                opened?;
                // The pump is open (again): whatever the reaper closed of it is the engine's once more.
                self.reaped.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
            }
        }
        // A venue whose tap refuses a stream (ibkr: a delayed quote is not a mark) says so here.
        watcher.door_price(venue, symbol)
    }

    fn price_made_ms(&self, venue: &str, symbol: &str, px: f64) -> Option<i64> {
        self.watcher(venue).ok()?.door_price_made_ms(venue, symbol, px)
    }

    fn top_of_book(&self, _route_key: &str, venue: &str, symbol: &str) -> Option<(f64, f64)> {
        self.watcher(venue).ok()?.door_top(venue, symbol)
    }

    fn last_trade(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.watcher(venue).ok()?.last_trade(venue, symbol)
    }

    /// A price alert's lanes: opened like a mount's, never counted as a paper book's price source.
    fn keep_bars(&self, wanted: &[SeriesKey]) {
        self.open_bar_lanes(wanted);
    }

    fn lane_verdict(&self, _route_key: &str, venue: &str, symbol: &str) -> Option<String> {
        let since = self.watcher(venue).ok()?.gap_since(venue, symbol)?;
        let down_s = vike_model::now_ms().saturating_sub(since).max(0) / 1_000;
        Some(format!(
            "its price stream is DOWN ({venue}'s stream for it has been in a gap for {down_s} s: the venue \
             refused the subscription or dropped the connection; the gatewayhub's log has the venue's words)"
        ))
    }

    fn admitted(&self, route_key: &str, venue: &str, symbol: &str) {
        // The engine's OWN symbol, re-rated at the door ([`SymbolAdmitter::own_symbol_rate`]), is no admission: no
        // slot of the engine's sixteen, no price lane of this registry's, and the daemon's reconcile client
        // already reads the mounted symbol, so it is told nothing.
        if self.is_foreign_mount(route_key, symbol) {
            return;
        }
        // An admission of a symbol being given back is the newer word: whatever the engine did with the earlier
        // statement, the symbol is wanted again, and `settle_releases` must not forget it (decision 0205). BEFORE
        // the insert: `forget_admitted` reads this table and removes from the set under one guard, so an insert it
        // can undo is one whose removal here it has already seen.
        self.releasing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(route_key.to_string(), symbol.to_string()));
        self.admitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((route_key.to_string(), symbol.to_string()));
        // An admitted symbol is the ENGINE's to bound (`MAX_EXTRA_SYMBOLS` per account); this map
        // counts only the subscriptions of symbols that are not admitted (yet).
        self.watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(venue.to_string(), symbol.to_string()));
        // The admission is REAL now (`AdmitSymbol` was sent, at the door or at boot), so tell the
        // daemon: its reconcile client reads this symbol from the next pass on. Every lookup before
        // this was a probe and told it nothing, so a Preview or a refused order never takes one of
        // its sixteen slots. One request per admission, with no lock held, and none for a symbol the
        // daemon holds a confirmed leverage for already ([`Self::settled`]: the seconds-long commit is not
        // repeated at every facts refresh); a failure is a warning, and the symbol is then read only after
        // the next commit.
        if self.settled(route_key, symbol) {
            return;
        }
        if let Some(Err(why)) = self.commit_to_daemon(route_key, symbol) {
            tracing::warn!(
                %route_key, %symbol,
                "the gatewayhub was not told this symbol is admitted ({why}); its reconcile \
                 client will not read it until the next admission request succeeds"
            );
        }
    }

    fn tell_daemon(&self, route_key: &str, _venue: &str, symbol: &str) -> Result<(), String> {
        // Only the commit: the set entry and the pump bookkeeping were made by `admitted` when the
        // admission was sent, and a stopped engine's were pruned (the caller checked the generation).
        // A daemon that was told already took it: the retry that follows `admitted` in the same round does not
        // pay the commit twice.
        if self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .is_some_and(|session| session.has_told(symbol))
        {
            return Ok(());
        }
        match self.commit_to_daemon(route_key, symbol) {
            Some(told) => told,
            None => Err(format!("`{route_key}` has no open trade lease to tell `{symbol}` over")),
        }
    }

    fn confirmed_leverage(&self, route_key: &str, symbol: &str) -> Option<f64> {
        self.leverage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(route_key.to_string(), symbol.to_string()))
            .copied()
    }

    /// Never waits and never refuses: the claim is a lock, and the lane (a lease look-up and a datahub
    /// subscribe) is opened on a thread of its own, as the boot's twin warm-up does.
    fn priced_by_mount(&self, venue: &str, symbol: &str) -> bool {
        self.mounted.get().is_some_and(|mounted| mounted(venue, symbol))
    }

    fn demo_login(&self, venue: &str) -> bool {
        self.demo_login_key(venue).is_some()
    }

    /// A boot mount on the pair ([`Self::bind_mounted`], [`Self::bind_boot_bars`]) or a bar lane the keeper opened or is
    /// opening for one since ([`Self::keep_bars`]). A lane only a price alert asked for does not count (its candles
    /// carry no quotes: `mount_bars`' doc).
    fn bar_series(&self, venue: &str, symbol: &str) -> bool {
        let of_pair = |(v, s, _): &SeriesKey| v == venue && s == symbol;
        let for_a_mount = |key: &SeriesKey| {
            self.mount_bars.lock().unwrap_or_else(PoisonError::into_inner).contains(key)
        };
        self.mounted.get().is_some_and(|mounted| mounted(venue, symbol))
            || self.boot_bars.get().is_some_and(|series| series.iter().any(of_pair))
            || self.bars.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(key, hold)| {
                of_pair(key) && matches!(hold, BarHold::Open | BarHold::Opening) && for_a_mount(key)
            })
    }

    fn hold_lane(&self, route_key: &str, venue: &str, symbol: &str) {
        let Some((generation, after_refusal)) = self.claim_hold(route_key, venue, symbol) else {
            return;
        };
        let Some(me) = self.me.get().and_then(std::sync::Weak::upgrade) else {
            // No keeper runs on this registry, so nothing could open the lane: the claim goes back.
            self.unclaim_hold(route_key, symbol);
            return;
        };
        let (rk, v, s) = (route_key.to_string(), venue.to_string(), symbol.to_string());
        let spawned = std::thread::Builder::new().name("lane-hold".into()).spawn(move || {
            me.open_hold(&rk, &v, &s, generation, after_refusal);
        });
        if let Err(e) = spawned {
            self.unclaim_hold(route_key, symbol);
            tracing::warn!(
                %route_key, %symbol,
                "could not start the thread that opens the price lane of the engine's own symbol ({e}); the \
                 lane keeper asks again at its next tick"
            );
        }
    }

    fn release_requested(&self, route_key: &str, _venue: &str, symbol: &str) {
        self.releasing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((route_key.to_string(), symbol.to_string()), Instant::now());
    }

    fn release(&self, route_key: &str, venue: &str, symbol: &str) {
        let admitted = self
            .admitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(route_key.to_string(), symbol.to_string()));
        if admitted {
            return;
        }
        // The pump a RUNNING lane twin warmed is the twin's, whoever else asked for the symbol and was refused
        // (v0.1.80 review b: a second binance perp account's refused order on `BTCUSDT.P` left the shared pump
        // idle, and the reaper closed it under the twin). It goes idle when the twin stops
        // ([`Self::engine_stopped`]).
        // …and so is the pump an engine holds for its OWN symbol ([`Self::hold_price`]).
        if self.warmed_on_venue(venue, symbol) || self.held_on_venue(venue, symbol) {
            return;
        }
        // The pump stays open and keeps its place under [`MAX_WATCHED`] for [`IDLE_TTL`], so the
        // operator's retry finds it; [`Self::close_idle_pumps`] closes it after that (under an open venue
        // gap too: the close says `Live` for the symbol, so the core's link dead-man window closes with it).
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sub) = watched.get_mut(&(venue.to_string(), symbol.to_string()))
            && *sub == Sub::Open
        {
            *sub = Sub::Idle(Instant::now());
        }
    }

    fn own_symbol_rate(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        engine: &VenueBlock,
    ) -> Option<SymbolAdmission> {
        let mount =
            self.mounts.lock().unwrap_or_else(PoisonError::into_inner).get(route_key)?.clone();
        if mount.symbol != symbol {
            return None;
        }
        // The engine's own word on how old its rate for the mount's currency is, as published (the node's clock:
        // `rate_as_of_ms` is stamped with it). A fresh one is left alone; a missing, stale or aged one is re-read.
        let now_ms = vike_model::now_ms();
        let fresh_ms = i64::try_from(RATE_FRESH_FOR.as_millis()).unwrap_or(i64::MAX);
        let kept = engine
            .quote_rates
            .iter()
            .find(|row| &*row.currency == mount.currency.as_str())
            .is_some_and(|row| !row.stale && now_ms.saturating_sub(row.as_of_ms) < fresh_ms);
        if kept {
            return None;
        }
        match self.lookup(route_key, venue, symbol) {
            Ok(fresh) if fresh.quote_currency.as_deref() == Some(mount.currency.as_str()) => {
                // The timer's own next read is a whole period away from THIS read.
                self.mount_next
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(route_key.to_string(), (Instant::now() + MOUNT_RATE_EVERY, 0));
                Some(fresh)
            }
            Ok(other) => {
                tracing::warn!(
                    %route_key, %symbol,
                    "the venue's facts for the engine's own symbol state {} and the mount is quoted in {}; the \
                     order is judged on the rate the engine has",
                    other.quote_currency.as_deref().unwrap_or("no currency"),
                    mount.currency
                );
                None
            }
            Err(why) => {
                tracing::warn!(
                    %route_key, %symbol,
                    "could not re-read the conversion rate of the engine's own symbol at the door ({}); the order \
                     is judged on the rate the engine has (an opening one is denied `quote-rate-stale` once it is \
                     120 s old, a closing one never)",
                    why.text()
                );
                None
            }
        }
    }

    fn refresh(&self, route_key: &str, venue: &str, symbol: &str) -> Option<SymbolAdmission> {
        let key = (route_key.to_string(), symbol.to_string());
        if !self.admitted.lock().unwrap_or_else(PoisonError::into_inner).contains(&key) {
            return None;
        }
        let due = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .is_none_or(|(at, a)| at.elapsed() >= refresh_after(a));
        if !due {
            return None;
        }
        self.reread(route_key, venue, symbol)
    }
}

impl AccountReadiness for TradeLeaseRegistry {
    fn exposure_cap_armed(&self, route_key: &str) -> bool {
        let hot = self
            .engines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .and_then(|state| state.cap);
        hot.unwrap_or_else(|| self.exposure.get().is_some_and(|armed| armed(route_key)))
    }

    fn awaiting_balance(&self, route_key: &str) -> Option<BalanceWait> {
        let engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        let (cell, since) = engines.get(route_key)?.awaiting.as_ref()?;
        (!cell.landed()).then(|| BalanceWait { since: since.elapsed(), why: cell.why() })
    }

    fn balance_held(&self) -> Vec<(String, String)> {
        let held = self.held_balances.lock().unwrap_or_else(PoisonError::into_inner);
        held.iter().map(|(route_key, reason)| (route_key.clone(), reason.clone())).collect()
    }

    fn start_refusal(&self, route_key: &str) -> Option<String> {
        self.start_refusals.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned()
    }

    fn generation(&self, route_key: &str) -> u64 {
        TradeLeaseRegistry::generation(self, route_key)
    }

    fn real_account_under_twin_label(&self, route_key: &str) -> bool {
        self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner).real.contains(route_key)
    }

    fn lane_twin_closing_only(&self, route_key: &str) -> bool {
        self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner).closing.contains(route_key)
    }

    fn derived_lane_twin(&self, route_key: &str) -> bool {
        self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner).derived.contains(route_key)
    }

    fn configured_accounts(&self, venue: &str) -> Option<Result<Vec<String>, String>> {
        let flags = self.twin_flags.lock().unwrap_or_else(PoisonError::into_inner);
        flags.configured.as_ref().map(|configured| match configured {
            Ok(by_venue) => Ok(by_venue.get(venue).cloned().unwrap_or_default()),
            Err(why) => Err(why.clone()),
        })
    }
}

impl LinkHealth for TradeLeaseRegistry {
    fn venue_link_down(&self) -> Vec<VenueLinkDownWire> {
        self.venue_links.lock().unwrap_or_else(PoisonError::into_inner).values().cloned().collect()
    }

    fn venue_path_up(&self) -> Vec<String> {
        let heard = self.venue_paths_up.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = heard.iter().filter(|(_, at)| at.elapsed() <= VENUE_PATH_UP_FRESH);
        fresh.map(|(route_key, _)| route_key.clone()).collect()
    }

    fn price_dials_up(&self) -> Option<u32> {
        self.price_links.get().map(|links| links.up())
    }

    /// Whether a lease is up is the SESSION's state; the `trade_links` map only adds the words and the instant, so a
    /// late word about an old lease cannot list a new one. A lease this node is ending itself (a stop, a teardown) is
    /// no loss, unless the node said why the engine lost it ([`TradeLeaseRegistry::set_trade_link_lost`]).
    fn trade_link_down(&self) -> Vec<TradeLinkDownWire> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let noted = self.trade_links.lock().unwrap_or_else(PoisonError::into_inner);
        let mut down: Vec<TradeLinkDownWire> = sessions
            .iter()
            .filter_map(|(route_key, session)| {
                let closed = match session.link_state() {
                    TradeLinkState::Up => return None,
                    TradeLinkState::Down => false,
                    TradeLinkState::Closed => true,
                };
                let note = noted.get(route_key);
                let ending_it = session.is_stopping();
                if closed && ending_it && note.is_none() {
                    return None;
                }
                let said = note.map(|(_, why)| why.clone());
                let why = if ending_it { said } else { session.closed_reason().or(said) };
                Some(TradeLinkDownWire {
                    route_key: route_key.clone(),
                    why: frame_words(&why.unwrap_or_default()),
                    down_ms: note.map_or(0, |(since, _)| {
                        u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
                    }),
                    closed,
                })
            })
            .collect();
        down.sort_by(|a, b| a.route_key.cmp(&b.route_key));
        down
    }

    fn price_link_down(&self) -> Vec<PriceLinkDownWire> {
        self.price_links.get().map_or_else(Vec::new, |links| links.lost())
    }
}

impl VenueReads for TradeLeaseRegistry {
    /// The operator's READ over `route_key`'s open lease, one round trip with no lock held. Reads
    /// the registry only: it asks the daemon nothing about symbols, registers nothing, tells the daemon's
    /// reconcile client nothing and keeps nothing of the answer.
    fn venue_read(
        &self,
        route_key: &str,
        kind: VenueReadKind,
    ) -> Result<VenueRead, vike_utility::errors::CarriedRefusal> {
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned();
        let Some(session) = session else {
            return Err(format!(
                "{route_key}: this node holds no open trade lease for the account, so it is not \
                 running here and there is no path to its venue (`vike-cli trade health` says why \
                 an account is not running)"
            )
            .into());
        };
        session.venue_read(kind, VENUE_READ_BOUND)
    }

    /// The wire ids over `route_key`'s open lease, one paged ask with no lock held, at [`WIRE_IDS_BOUND`].
    fn wire_ids(
        &self,
        route_key: &str,
        order_client_ids: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned();
        let Some(session) = session else {
            return Err(format!(
                "{route_key}: this node holds no open trade lease for the account"
            ));
        };
        session.wire_pairs(order_client_ids, WIRE_IDS_BOUND).map_err(|e| e.text().to_string())
    }
}

impl TradeLeaseRegistry {
    /// The facts of an admitted symbol read AGAIN, whatever their age: the body of [`SymbolAdmitter::refresh`] once
    /// it is due, and what the rate timer ([`Self::refresh_held_rates`]) asks. `None` when the lookup failed,
    /// said at `warn`, with the next try pushed back (a minute for facts, [`RATE_RETRY_AFTER`] for a rate)
    /// instead of repeated on every order while the venue is failing.
    fn reread(&self, route_key: &str, venue: &str, symbol: &str) -> Option<SymbolAdmission> {
        let key = (route_key.to_string(), symbol.to_string());
        match self.lookup(route_key, venue, symbol) {
            Ok(fresh) => Some(fresh),
            Err(why) => {
                let rated = self
                    .answers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_mut(&key)
                    .map(|entry| {
                        let rated = entry.1.quote_rate.is_some();
                        let rewind = if rated {
                            RATE_FRESH_FOR - RATE_RETRY_AFTER
                        } else {
                            REFRESH_AFTER - RETRY_AFTER
                        };
                        if let Some(at) = Instant::now().checked_sub(rewind) {
                            entry.0 = at;
                        }
                        rated
                    })
                    .unwrap_or(false);
                if rated {
                    tracing::warn!(
                        %route_key, %symbol,
                        "could not re-read an admitted symbol's conversion rate ({why}); the engine \
                         keeps the rate it has, and an opening order is denied once that is 120 s old"
                    );
                } else {
                    tracing::warn!(
                        %route_key, %symbol,
                        "could not refresh an admitted symbol's facts ({why}); the engine keeps its grid"
                    );
                }
                None
            }
        }
    }

    /// The admitted symbols this registry last priced with a conversion rate, as `(route key, symbol)`: the
    /// ones the rate timer keeps fresh. **Least recently read first** (a failed read is rewound to look older
    /// still, `reread`), ties broken by name: when the venue's own budget runs out part-way down the list (IG
    /// answers 12 lookups a minute per account), the symbols that go without are the ones that were read last
    /// time, so the budget rotates and no symbol is the one that always loses (an alphabetical order made the
    /// last name lose every tick, and its rate went stale: opening orders denied).
    pub fn rated_symbols(&self) -> Vec<(String, String)> {
        let admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let mut rated: Vec<(Instant, (String, String))> = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(key, (_, a))| a.quote_rate.is_some() && admitted.contains(*key))
            .map(|(key, (at, _))| (*at, key.clone()))
            .collect();
        rated.sort();
        rated.into_iter().map(|(_, key)| key).collect()
    }

    /// **One tick of the rate timer, run whole and joined** (decision 0183, phase 1): for every admitted symbol this
    /// registry holds a rate for that `held` says the account still has something in (the production closure asks
    /// for a non-zero position; a resting order alone does not count), read the
    /// venue's rate again and hand the fresh admission to `send`, which puts it on the core's lane. Returns how
    /// many were sent. A symbol that is not held is left alone (the order path re-reads its rate before the next
    /// order is judged); one whose re-read failed keeps the rate the engine has and is tried again at the next
    /// tick, soon ([`RATE_RETRY_AFTER`]). Nothing here is the door: no price is asked and the daemon is not told
    /// anything, the symbol is already admitted.
    ///
    /// ⚠ **This is the synchronous form, for a caller that wants the whole pass finished** (the tests): the running
    /// node's timer ([`Self::spawn_rate_refresher`]) does not join ticks. It shares this tick's two halves,
    /// [`Self::rate_shares`] (what each venue has to read) and [`Self::refresh_venue`] (the read), and hands the
    /// shares to long-lived per-venue workers instead of scoped threads, so one slow venue can no longer delay
    /// the NEXT tick of every venue (A53).
    ///
    /// **Each venue's symbols are read on a thread of their own**: a slow venue (an IBKR gateway that takes seconds
    /// per quiet pair) delays its own symbols only, never another venue's (oanda's rates must land inside the
    /// engine's 120 s window whatever ibkr is doing). What a venue's thread read is handed to `send`, on the
    /// calling thread, THE MOMENT THAT THREAD FINISHES, not when the slowest one has: a venue's rates reach the
    /// engine as soon as they are read, in the order the venues finish. A thread the OS refuses to start is not
    /// a dead timer: that venue's share runs on the calling thread instead, and the threads that did start still
    /// run beside it. All the threads are joined before the tick ends.
    ///
    /// Two more duties, both for a venue that has such a symbol and no one else: the symbols the journal says the
    /// restored engine held in another currency, that are held and have no rated answer (the boot's
    /// re-admission missed them and its retry gave up) are re-admitted by the boot's own retry round
    /// ([`crate::readmit::retry_round_quiet`]), so a restored position is not valued at its restored rate for as
    /// long as it is held: at most ONE such symbol per venue per tick (each try asks the venue's gateway, and for
    /// ibkr its data lane, rationed by the hour), a failed one waits 1, 2, 4, 8 and then 15 minutes
    /// before its next try ([`OwedBackoff`]), and the failure is said once, at `error`, when it first fails, and
    /// the heal once, at `info`; and BEFORE the pass, every held foreign-quoted position whose rate is older than
    /// [`RATE_UNREFRESHED_AFTER`] is named at `warn` with its age ([`Self::unrefreshed_held`]) - the previous
    /// tick's failure, since the engine has folded what the previous tick sent by now.
    pub fn refresh_held_rates(
        &self,
        held: &(dyn Fn(&str, &str) -> bool + Sync),
        send: &mut dyn FnMut(Command),
    ) -> usize {
        let venues = self.rate_shares(held);
        let mut sent = 0;
        std::thread::scope(|scope| {
            // ONE channel for the whole tick: a venue's thread puts what it read on it when it is done, and this
            // thread sends it on at once.
            let (done, finished) = std::sync::mpsc::channel::<Vec<Command>>();
            let mut threads = Vec::new();
            let mut inline = Vec::new();
            for (venue, (rated, owed)) in &venues {
                let done = done.clone();
                let started = std::thread::Builder::new()
                    .name(format!("lease-facts-rates:{venue}"))
                    .spawn_scoped(scope, move || {
                        let _ = done.send(self.refresh_venue(venue, rated, owed, held));
                    });
                match started {
                    Ok(thread) => threads.push(thread),
                    Err(e) => {
                        tracing::warn!(
                            %venue,
                            "could not start the conversion-rate thread of {venue} ({e}); this tick reads its \
                             share on the timer's own thread"
                        );
                        inline.push((venue, rated, owed));
                    }
                }
            }
            drop(done);
            for (venue, rated, owed) in inline {
                for command in self.refresh_venue(venue, rated, owed, held) {
                    send(command);
                    sent += 1;
                }
            }
            // Ends when every venue's thread has put its share on the channel or died without one.
            while let Ok(batch) = finished.recv() {
                for command in batch {
                    send(command);
                    sent += 1;
                }
            }
            // A venue's thread that panicked is joined here (an unjoined one would panic the scope): its symbols
            // keep the rate the engine has, and the next tick tries again.
            for thread in threads {
                if thread.join().is_err() {
                    tracing::error!(
                        "a conversion-rate thread panicked; its symbols keep the rate they have"
                    );
                }
            }
        });
        sent
    }

    /// **What one tick of the rate timer has to do, by venue**: every venue's rated symbols and journaled symbols
    /// nothing has rated yet ([`VenueShare`]). The part of a tick that is the same however it is run, whole and
    /// joined ([`Self::refresh_held_rates`]) or handed to the venues' long-lived workers
    /// ([`Self::spawn_rate_refresher`]): it first names, at `warn`, the held foreign-quoted positions whose rate is
    /// not being refreshed ([`Self::say_unrefreshed`]) and forgets the backoff of a symbol no longer owed.
    fn rate_shares(
        &self,
        held: &(dyn Fn(&str, &str) -> bool + Sync),
    ) -> BTreeMap<String, VenueShare> {
        self.say_unrefreshed(held);
        let rated = self.rated_symbols();
        let owed = self.owed_symbols(&rated);
        // A symbol that is no longer owed (rated by an order, its engine stopped) leaves no state behind.
        self.owed_backoff
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|pair, _| owed.contains(pair));
        let mut venues: BTreeMap<String, VenueShare> = BTreeMap::new();
        let venue_of = |route_key: &str| venue_of_route_key(route_key).to_string();
        for pair in rated {
            venues.entry(venue_of(&pair.0)).or_default().0.push(pair);
        }
        for pair in owed {
            venues.entry(venue_of(&pair.0)).or_default().1.push(pair);
        }
        venues
    }

    /// One venue's share of a tick: the fresh admissions of its held rated symbols, and the heal of its held
    /// journaled symbols nothing has rated yet. Returns the commands to send.
    fn refresh_venue(
        &self,
        venue: &str,
        rated: &[(String, String)],
        owed: &[(String, String)],
        held: &(dyn Fn(&str, &str) -> bool + Sync),
    ) -> Vec<Command> {
        let mut out = Vec::new();
        for (route_key, symbol) in rated {
            if held(route_key, symbol)
                && let Some(fresh) = self.reread(route_key, venue, symbol)
            {
                out.push(Command::AdmitSymbol(Box::new(fresh)));
            }
        }
        // At most ONE owed symbol is tried per venue per tick, and none before its backoff has run: each try asks
        // the venue's gateway (for ibkr, a dial of its data lane, rationed by the hour), so every owed symbol every
        // minute would drain the lane and refuse every new symbol until the hour rolled.
        let mut tried = false;
        for (route_key, symbol) in owed {
            if tried || !held(route_key, symbol) || !self.owed_due(route_key, symbol) {
                continue;
            }
            tried = true;
            let missed = crate::readmit::ReadmitMiss {
                target: crate::readmit::ReadmitTarget {
                    route_key: route_key.clone(),
                    venue: venue.to_string(),
                    symbol: symbol.clone(),
                    flat: false,
                },
                told: false,
                generation: self.generation(route_key),
            };
            let (still, why) = crate::readmit::retry_round_quiet(&[missed], self, |c| out.push(c));
            self.owed_tried(route_key, symbol, still.is_empty(), why.as_deref());
        }
        out
    }

    /// Whether the rate timer may try the owed `symbol` of `route_key` now (it never failed, or its backoff ran).
    fn owed_due(&self, route_key: &str, symbol: &str) -> bool {
        self.owed_backoff
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(route_key.to_string(), symbol.to_string()))
            .is_none_or(|state| state.due(Instant::now()))
    }

    /// Record the outcome of the timer's try of an owed symbol, and say it ONCE per change of state: the first
    /// failure at `error` (with the reason), the heal at `info`; the tries between are silent (`debug`), so a
    /// symbol that stays unhealable costs one line, not one a minute.
    fn owed_tried(&self, route_key: &str, symbol: &str, healed: bool, why: Option<&str>) {
        let key = (route_key.to_string(), symbol.to_string());
        let mut states = self.owed_backoff.lock().unwrap_or_else(PoisonError::into_inner);
        if healed {
            let was = states.remove(&key);
            drop(states);
            tracing::info!(
                %route_key, %symbol,
                "the rate timer re-admitted a restored foreign-quoted position the boot had missed{}; \
                 its conversion rate is fresh again",
                was.map_or(String::new(), |w| format!(" (after {} failed tries)", w.fails))
            );
            return;
        }
        let first = self.owed_retry_first.get().copied().unwrap_or(OWED_RETRY_FIRST);
        let state = OwedBackoff::failed(states.remove(&key), Instant::now(), first);
        let fails = state.fails;
        states.insert(key, state);
        drop(states);
        if fails == 1 {
            tracing::error!(
                %route_key, %symbol,
                "the rate timer could not re-admit a restored foreign-quoted position the boot had missed ({}): its \
                 conversion rate stays the restored one. It tries again after {} s, doubling up to {} min, one try \
                 per tick, and says nothing more until it heals",
                why.unwrap_or("no reason given"),
                first.as_secs(),
                (first * OWED_RETRY_CAP_FACTOR).as_secs() / 60
            );
        } else {
            tracing::debug!(%route_key, %symbol, fails, "the owed symbol is still not re-admitted");
        }
    }

    /// Shorten [`OWED_RETRY_FIRST`] (once; for a test that must not wait a minute).
    pub fn set_owed_retry_first(&self, first: Duration) {
        let _ = self.owed_retry_first.set(first);
    }

    /// The journaled foreign-quoted symbols no answer has rated yet (`rated` is [`Self::rated_symbols`]), as
    /// `(route key, symbol)`, sorted.
    fn owed_symbols(&self, rated: &[(String, String)]) -> Vec<(String, String)> {
        let mut owed: Vec<(String, String)> = self
            .journaled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .flat_map(|(route_key, symbols)| {
                symbols.iter().map(move |(symbol, _)| (route_key.clone(), symbol.clone()))
            })
            .filter(|pair| !rated.contains(pair))
            .collect();
        owed.sort();
        owed
    }

    /// **The held foreign-quoted positions whose conversion rate is not being kept fresh**: every symbol the
    /// registry rated or the journal says was restored in another currency, that `held` says the account still
    /// has something in, whose engine rate (as the core publishes it, [`Self::bind_rate_rows`]) is older than
    /// [`RATE_UNREFRESHED_AFTER`] at `now_ms` or missing. The engine keeps valuing such a position (equity,
    /// unrealized PnL, the margin sweep) at that rate and denies opening orders on it from 120 s; nothing on the
    /// node side can change that but a successful re-read, so this names them. Empty while the rows are unbound.
    pub fn unrefreshed_held(
        &self,
        held: &(dyn Fn(&str, &str) -> bool + Sync),
        now_ms: i64,
    ) -> Vec<Unrefreshed> {
        let Some(rows) = self.rate_rows.get() else { return Vec::new() };
        let mut tracked: Vec<(String, String, String)> = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, (_, a))| a.quote_rate.is_some())
            .map(|((route_key, symbol), (_, a))| {
                (route_key.clone(), symbol.clone(), a.quote_currency.clone().unwrap_or_default())
            })
            .collect();
        let admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner).clone();
        tracked.retain(|(route_key, symbol, _)| {
            admitted.contains(&(route_key.clone(), symbol.clone()))
        });
        for (route_key, symbols) in
            self.journaled.lock().unwrap_or_else(PoisonError::into_inner).iter()
        {
            for (symbol, currency_value) in symbols {
                if !tracked
                    .iter()
                    .any(|(rk, symbol_name, _)| rk == route_key && symbol_name == symbol)
                {
                    tracked.push((route_key.clone(), symbol.clone(), currency_value.clone()));
                }
            }
        }
        tracked.sort();
        let limit = i64::try_from(RATE_UNREFRESHED_AFTER.as_millis()).unwrap_or(i64::MAX);
        tracked
            .into_iter()
            .filter(|(route_key, symbol, _)| held(route_key, symbol))
            .filter_map(|(route_key, symbol, currency_value)| {
                let as_of = rows(&route_key, &currency_value);
                let age_ms = as_of.map(|at| now_ms.saturating_sub(at));
                if age_ms.is_none_or(|age| age > limit) {
                    Some(Unrefreshed {
                        route_key,
                        symbol,
                        currency: currency_value,
                        age_s: age_ms.map(|ms| ms / 1_000),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Say at `warn`, once per call, which held foreign-quoted positions are valued at a rate nothing is
    /// refreshing ([`Self::unrefreshed_held`]).
    fn say_unrefreshed(&self, held: &(dyn Fn(&str, &str) -> bool + Sync)) {
        let stale = self.unrefreshed_held(held, vike_model::now_ms());
        if stale.is_empty() {
            return;
        }
        let names: Vec<String> = stale
            .iter()
            .map(|u| match u.age_s {
                Some(age) => {
                    format!("{} {} ({} rate {age} s old)", u.route_key, u.symbol, u.currency)
                }
                None => {
                    format!(
                        "{} {} ({} rate: the engine holds none)",
                        u.route_key, u.symbol, u.currency
                    )
                }
            })
            .collect();
        tracing::warn!(
            symbols = ?names,
            "{} held foreign-quoted position(s) are valued at a conversion rate that is NOT being refreshed: {}. \
             The engine keeps valuing them (equity, unrealized PnL, the margin sweep) at the rate it has and \
             denies an opening order in them once it is 120 s old; the venue's rate source (for ibkr: the \
             gateway's data connection and forex quote) has not answered. This line repeats every minute \
             until a re-read lands",
            stale.len(),
            names.join(", ")
        );
    }

    /// Whether the boot re-admission window is open ([`Self::begin_boot`]).
    fn boot_open(&self) -> bool {
        self.boot.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Start the timer thread that hands every venue its share of a tick ([`Self::rate_shares`]) every `every`
    /// until the registry is dropped (it holds only a `Weak`): the rate of a foreign-quoted position is re-read
    /// about once a minute with no order to carry it (the engine calls a rate stale at 120 s). `held` is asked
    /// from the venues' workers and the timer, and `send` is the core's lossless lane, called on the timer thread
    /// alone.
    ///
    /// **Each venue has a LONG-LIVED worker of its own** ([`RatePool`], A53): the timer never reads a rate, it
    /// hands a venue's share to that venue's worker and goes on, so a venue whose read outlasts `every` (an IBKR
    /// gateway that takes seconds per quiet pair) delays NOTHING of any other venue's, not even the next tick:
    /// the tick that finds the slow venue still reading skips it (and the worker reads its share once more, at
    /// once, when it is done, so a slow venue is never a whole period behind the clock). What a worker read is
    /// sent the moment it is done. The workers end with the registry, and a worker the OS refuses to start is not
    /// a dead timer: that venue's share runs on the timer's own thread instead, as before.
    ///
    /// **The ticks keep a fixed rate**: a tick starts `every` after the previous one STARTED, not after it ended.
    /// **The timer waits out the boot window** ([`Self::begin_boot`], looked at again each [`BOOT_RECHECK`]): a
    /// tick that ran inside it would take the boot bound for its asks and could mark a route key `missed` for
    /// the boot's remaining symbols.
    pub fn spawn_rate_refresher(
        self: &Arc<Self>,
        every: Duration,
        held: impl Fn(&str, &str) -> bool + Send + Sync + 'static,
        mut send: impl FnMut(Command) + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        let held: Arc<HeldCheck> = Arc::new(held);
        let spawned =
            std::thread::Builder::new().name("lease-facts-rates".into()).spawn(move || {
                let mut pool = RatePool::new(weak.clone(), Arc::clone(&held));
                std::thread::sleep(every);
                loop {
                    let Some(facts) = weak.upgrade() else { return };
                    if facts.boot_open() {
                        drop(facts);
                        std::thread::sleep(BOOT_RECHECK);
                        continue;
                    }
                    let began = Instant::now();
                    for (venue, share) in facts.rate_shares(&*held) {
                        if let Handed::Failed((rated, owed)) = pool.hand_over(&venue, share) {
                            for command in facts.refresh_venue(&venue, &rated, &owed, &*held) {
                                send(command);
                            }
                        }
                    }
                    // Not held across the wait: the thread ends when the registry is dropped.
                    drop(facts);
                    pool.send_finished_until(began + every, &mut send);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the conversion-rate refresher ({e}); a foreign-quoted position's rate \
                 is re-read by an order on it only, and goes stale (opening orders denied) after 120 s"
            );
        }
    }
}

// ---- the engine's OWN symbol booked through a rate (decision 0183, the mounted-symbol amendment) ---------

impl TradeLeaseRegistry {
    /// **Note that `route_key`'s engine is mounted on `symbol`, quoted in `currency`, a currency other than its ledger's,
    /// and that the venue's lane rates it** (`VenueBackendGateway::start_adapter`, after [`Self::anchor_currency`] of the
    /// same start): the engine's assembly declares such a mount foreign-quoted with no rate, so this registry keeps
    /// it rated: [`Self::rate_foreign_mounts`] reads it at the next tick and about every [`MOUNT_RATE_EVERY`] after,
    /// and the door re-rates it when an order meets a rate that has aged ([`SymbolAdmitter::own_symbol_rate`]).
    /// Replaces the account's earlier entry (a start decides afresh).
    pub fn note_foreign_mount(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        currency_value: &str,
    ) {
        let Some(currency_value) =
            vike_model::currency_code(currency_value).filter(|_| !symbol.trim().is_empty())
        else {
            return;
        };
        self.mounts.lock().unwrap_or_else(PoisonError::into_inner).insert(
            route_key.to_string(),
            MountQuote {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                currency: currency_value,
            },
        );
        // A mount just noted is due now.
        self.mount_next.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
    }

    /// Forget `route_key`'s mount (a new start, a stop, a refused start).
    fn forget_foreign_mount(&self, route_key: &str) {
        self.mounts.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
        self.mount_next.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
    }

    /// Whether `symbol` is `route_key`'s engine's mount booked through a rate ([`Self::note_foreign_mount`]).
    fn is_foreign_mount(&self, route_key: &str, symbol: &str) -> bool {
        self.mounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .is_some_and(|m| m.symbol == symbol)
    }

    /// **One tick of the mount rate timer**: for every engine whose mount is booked through a rate and whose next
    /// read is due (a mount just noted, or [`MOUNT_RATE_EVERY`] after its last good read, or [`RATE_RETRY_AFTER`]
    /// after a failed one), read the venue's facts for the mount and hand the fresh admission to `send`, which puts
    /// it on the core's lossless lane. Returns how many were sent.
    ///
    /// This is the START-TIME admission of the mount (the engine declared it with no rate, and denies an opening
    /// order on it until one lands), its refresh while the account runs (with or without a position and whether or
    /// not a strategy is mounted on it: a strategy's orders never meet the door) and the heal after a boot (the
    /// restore brings the snapshot's rate back stale). Nothing here is the door: no price is asked, no symbol slot
    /// is spent and the daemon is told nothing: the mount is the engine's own symbol, not an admission.
    ///
    /// A failed read keeps the rate the engine has and is tried again soon; the first failure of a run is said at
    /// `warn` (with the reason), the ones after it at `debug` and every thirtieth at `warn` again, and the heal at
    /// `info`, so a closed market costs a line, not a line every ten seconds. A venue whose answer no longer states
    /// the mount's currency is not sent (the engine would refuse it `quote-currency-missing`).
    pub fn rate_foreign_mounts(&self, send: &mut dyn FnMut(Command)) -> usize {
        let due: Vec<(String, MountQuote)> = {
            let now = Instant::now();
            let next = self.mount_next.lock().unwrap_or_else(PoisonError::into_inner);
            self.mounts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter(|(route_key, _)| next.get(*route_key).is_none_or(|(at, _)| now >= *at))
                .map(|(route_key, mount)| (route_key.clone(), mount.clone()))
                .collect()
        };
        let mut sent = 0;
        for (route_key, mount) in due {
            let read = match self.lookup(&route_key, &mount.venue, &mount.symbol) {
                Ok(fresh) if fresh.quote_currency.as_deref() == Some(mount.currency.as_str()) => {
                    Ok(fresh)
                }
                Ok(other) => Err(format!(
                    "the venue's facts for the mount `{}` state {} and the engine's mount is quoted in {}",
                    mount.symbol,
                    other.quote_currency.as_deref().unwrap_or("no currency"),
                    mount.currency
                )),
                Err(why) => Err(why.text().to_string()),
            };
            // The mount may have been dropped or replaced while the venue answered (a stop, a new start).
            if self.mounts.lock().unwrap_or_else(PoisonError::into_inner).get(&route_key)
                != Some(&mount)
            {
                continue;
            }
            let mut next = self.mount_next.lock().unwrap_or_else(PoisonError::into_inner);
            match read {
                Ok(fresh) => {
                    let failed = next.remove(&route_key).map_or(0, |(_, fails)| fails);
                    next.insert(route_key.clone(), (Instant::now() + MOUNT_RATE_EVERY, 0));
                    drop(next);
                    if failed > 0 {
                        tracing::info!(
                            %route_key, symbol = %mount.symbol,
                            "the mounted symbol's conversion rate is being read again (after {failed} failed read(s))"
                        );
                    }
                    send(Command::AdmitSymbol(Box::new(fresh)));
                    sent += 1;
                }
                Err(why) => {
                    let fails = next.get(&route_key).map_or(0, |(_, fails)| *fails) + 1;
                    next.insert(route_key.clone(), (Instant::now() + RATE_RETRY_AFTER, fails));
                    drop(next);
                    if fails == 1 || fails.is_multiple_of(30) {
                        tracing::warn!(
                            %route_key, symbol = %mount.symbol, fails,
                            "could not read the conversion rate of the engine's mounted symbol ({why}); the engine \
                             keeps the rate it has (none yet: an opening order on the mount is denied \
                             `quote-rate-stale`, a closing one never) and it is read again in {} s",
                            RATE_RETRY_AFTER.as_secs()
                        );
                    } else {
                        tracing::debug!(%route_key, symbol = %mount.symbol, fails, "{why}");
                    }
                }
            }
        }
        sent
    }

    /// Start the thread that calls [`Self::rate_foreign_mounts`] every `every` ([`MOUNT_RATE_TICK`]) until the
    /// registry is dropped (it holds only a `Weak`), waiting out the boot window like the held-rate timer
    /// ([`Self::spawn_rate_refresher`]): a tick inside it would take the boot bound for its asks.
    pub fn spawn_mount_rater(
        self: &Arc<Self>,
        every: Duration,
        mut send: impl FnMut(Command) + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-mount-rates".into()).spawn(move || {
                loop {
                    let Some(facts) = weak.upgrade() else { return };
                    if facts.boot_open() {
                        drop(facts);
                        std::thread::sleep(BOOT_RECHECK);
                        continue;
                    }
                    facts.rate_foreign_mounts(&mut send);
                    drop(facts);
                    std::thread::sleep(every);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the mounted-symbol rate refresher ({e}); an engine whose mount is booked through a \
                 conversion rate gets its rate from the orders that meet the door only, and goes stale (opening \
                 orders on the mount denied) after 120 s"
            );
        }
    }

    /// **State each PAPER engine's own epic's contract size, read over the venue's DEMO login** (the owner's ruling
    /// of 2026-11-16, [`vike_paper::unit_stated_over_demo_login`]). For the newest `paper` block of each route key on such a
    /// venue, with a symbol, not yet stated to its engine ([`Self::generation`]: a restarted engine is stated
    /// again), while a `demo` account of the venue holds a lease here, the symbol's facts are asked over THAT lease
    /// ([`Self::admission`]: the gatewayhub answers an ig epic from its kept market details, one IG read the first
    /// time and none after; the paper account never logs in and sends the venue nothing), and their contract value
    /// goes to `send` as the journaled `Command::AdmitSymbol` of the paper engine's mounted symbol: the engine books
    /// by it and tells its book (`vike_execution::Engine::admit_symbol`'s mounted arm), which then takes the epic's
    /// orders and fills them on the venue's bid and ask.
    ///
    /// Nothing is sent with no demo login: the book refuses every order of the epic in the owner's words (`no
    /// price: no demo login for ig`), and THIS is where that is said, at `error`, once (the paper start runs before
    /// any statement can exist, so it says nothing of it). Nor for an epic the facts refuse or whose money is not the
    /// paper book's one ledger currency (it would book another money at par): said at `warn` once per reason. Each is
    /// asked again next tick.
    ///
    /// **While the venue's market is closed at `now_ms` nothing is asked** (`crate::readmit::paper_waits_for_open`: the
    /// demo login itself does not log in before the open, so its absence is no fault): the account's wait is said ONCE
    /// at `info`, with the open, and for [`PAPER_UNITS_OPEN_GRACE`] after the open a missing demo login is still that
    /// login coming up and is said nothing of; the first tick that finds it states the size, with no restart.
    /// Returns how many were sent.
    pub fn state_paper_units(
        &self,
        blocks: &[VenueBlock],
        now_ms: i64,
        send: &mut dyn FnMut(Command),
    ) -> usize {
        let mut sent = 0;
        for (i, block) in blocks.iter().enumerate() {
            if block.tier != Some(vike_model::AccountTier::Paper)
                || block.symbol.is_empty()
                || !vike_paper::unit_stated_over_demo_login(&block.venue)
                || blocks[i + 1..].iter().any(|newer| newer.route_key == block.route_key)
            {
                continue;
            }
            let key = (block.route_key.clone(), block.symbol.clone());
            let generation = self.generation(&block.route_key);
            let tried =
                self.paper_units.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned();
            if tried.as_ref().is_some_and(|t| t.generation == generation && t.failed.is_none()) {
                continue;
            }
            let waits =
                |at: i64| crate::readmit::paper_waits_for_open(block.tier, &block.venue, at);
            let closed = waits(now_ms);
            // An epic's contract size is the same over any demo account of the venue: the lowest route key, stable.
            let demo = if closed.is_some() {
                None
            } else {
                self.demo_lease_key(Some(CredentialedStreamLogin::AnyDemoSession), &block.venue)
            };
            // The open has just come and the demo login the clock held back is still logging in: still the wait.
            let grace_ms = i64::try_from(PAPER_UNITS_OPEN_GRACE.as_millis()).unwrap_or(i64::MAX);
            let waiting = match (closed, &demo) {
                (None, None) => waits(now_ms.saturating_sub(grace_ms)),
                _ => closed,
            };
            if let Some(open) = waiting {
                let why = crate::server::symbol_admission::paper_market_closed(&block.venue, open);
                if tried.and_then(|t| t.failed).as_ref() != Some(&why) {
                    tracing::info!(
                        route_key = %block.route_key, symbol = %block.symbol,
                        "{}: waits for the open: {why}. One unit of `{}` is read over a {} demo login, which logs in \
                         at the open, and is stated then with no restart",
                        block.route_key.replacen('#', "/", 1), block.symbol, block.venue
                    );
                }
                self.paper_units
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key, PaperUnitTry { generation, failed: Some(why) });
                continue;
            }
            let Some(demo) = demo else {
                // No demo login: every order of the epic is refused, said ONCE, HERE, where it is known (the paper
                // START cannot know it: `vike_paper::unit_stated_over_demo_login`); asked again next tick.
                let venue = block.venue.as_str();
                let why = format!(
                    "{}: one unit of `{}` is read over a running {venue} demo login, and \
                     no {venue} demo account runs here, so its paper book refuses every \
                     order of it rather than book a guessed unit. Arm one \
                     (`account.active = 1` on its `demo` row)",
                    crate::server::symbol_admission::no_demo_login(venue),
                    block.symbol
                );
                if tried.and_then(|t| t.failed).as_ref() != Some(&why) {
                    tracing::error!(route_key = %block.route_key, symbol = %block.symbol, "{why}");
                }
                self.paper_units
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key, PaperUnitTry { generation, failed: Some(why) });
                continue;
            };
            let ledger = vike_model::DEFAULT_CURRENCY;
            let read = self
                .admission(&demo, &block.venue, &block.symbol)
                .map_err(|e| e.text().to_string())
                .and_then(|facts| {
                    // No currency named: the symbol is quoted in the demo account's own money.
                    let money = facts.quote_currency.clone().or_else(|| {
                        self.anchored.lock().unwrap_or_else(PoisonError::into_inner).get(&demo).cloned()
                    });
                    match money {
                        Some(money) if money == ledger => Ok(facts.contract_value.unwrap_or_default()),
                        other => Err(format!(
                            "the demo login states it is quoted in {}, and a paper book is one {ledger} ledger \
                             with no rate",
                            other.as_deref().unwrap_or("no currency")
                        )),
                    }
                });
            let mut kept = self.paper_units.lock().unwrap_or_else(PoisonError::into_inner);
            match read {
                Ok(value) => {
                    kept.insert(key, PaperUnitTry { generation, failed: None });
                    drop(kept);
                    send(Command::AdmitSymbol(Box::new(SymbolAdmission {
                        route_key: block.route_key.clone(),
                        symbol: block.symbol.clone(),
                        grid: SymbolGrid::default(),
                        contract_value: Some(value),
                        initial_mark: None,
                        initial_mark_ms: None,
                        quote_currency: None,
                        quote_rate: None,
                        rate_as_of_ms: None,
                        initial_margin_requirement: None,
                        release: false,
                        margin_mode: None,
                    })));
                    sent += 1;
                }
                Err(why) => {
                    if tried.and_then(|t| t.failed).as_ref() != Some(&why) {
                        tracing::warn!(
                            route_key = %block.route_key, symbol = %block.symbol, demo = %demo,
                            "{}: the paper account's `{}` takes no contract size from the demo login `{demo}` \
                             ({why}); its orders stay refused, and it is asked again",
                            block.venue, block.symbol
                        );
                    }
                    kept.insert(key, PaperUnitTry { generation, failed: Some(why) });
                }
            }
        }
        sent
    }

    /// Start the thread that calls [`Self::state_paper_units`] over the blocks `blocks` reads (the published
    /// snapshot's), on the wall clock, every `every` ([`MOUNT_RATE_TICK`]) until the registry is dropped (it holds
    /// only a `Weak`), waiting out the boot window like the mount rater ([`Self::spawn_mount_rater`]).
    pub fn spawn_paper_unit_stater(
        self: &Arc<Self>,
        every: Duration,
        blocks: impl Fn() -> Vec<VenueBlock> + Send + 'static,
        mut send: impl FnMut(Command) + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-paper-units".into()).spawn(move || {
                loop {
                    let Some(facts) = weak.upgrade() else { return };
                    if facts.boot_open() {
                        drop(facts);
                        std::thread::sleep(BOOT_RECHECK);
                        continue;
                    }
                    facts.state_paper_units(&blocks(), vike_model::now_ms(), &mut send);
                    drop(facts);
                    std::thread::sleep(every);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the paper contract-size stater ({e}); a paper ig account's orders stay refused \
                 (`no price: no demo login for ig`)"
            );
        }
    }
}

// ---- the held lane of an engine's OWN symbol (decision 0197) --------------------------------------------

impl TradeLeaseRegistry {
    /// **Bind the keyless facts look-up** (once, by the root): an account with NO trade lease whose route key the
    /// look-up answers for (a `paper` one: it never logs in) is admitted on the facts the datahub states for the
    /// symbol (`vike_datahub_client::strat::symbol_facts`: the store's recorded row, else the venue's public catalog
    /// row, decision 0285's amendment of 2026-11-16). Unbound, or `None` for the route key, such an account's look-up
    /// is the no-lease refusal, as ever.
    pub fn bind_keyless_facts(
        &self,
        keyless: impl Fn(
            &str,
            &str,
            &str,
        )
            -> Option<Result<vike_model::SymbolProperties, (FailureOutcome, String)>>
        + Send
        + Sync
        + 'static,
    ) {
        let _ = self.keyless.set(Box::new(keyless));
    }

    /// The admission of `symbol` on the keyless look-up's `facts` ([`Self::bind_keyless_facts`]): their grid, no rate
    /// (a paper ledger books the symbol in its own money), cached like a lease's answer. The datahub's answer refusing
    /// it is refused in its words and remembered like any refusal ([`NEGATIVE_TTL`]), so a flood of orders is not a
    /// flood of reads; a link that carried no answer (a datahub restarting) says nothing about the symbol, so it is
    /// `Unavailable` and never remembered: the next order asks again.
    fn keyless_admission(
        &self,
        facts: Result<vike_model::SymbolProperties, (FailureOutcome, String)>,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        let facts = facts.map_err(|(outcome, why)| {
            let why = format!(
                "`{symbol}` on {venue} cannot be admitted for this paper account, which holds no trade lease: {why}"
            );
            match outcome {
                FailureOutcome::Refused => SymbolPropertiesRequestError::Refused(why),
                _ => SymbolPropertiesRequestError::Unavailable(why),
            }
        })?;
        let admission = SymbolAdmission {
            route_key: route_key.to_string(),
            symbol: symbol.to_string(),
            grid: SymbolGrid::from_properties(&facts),
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        };
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).insert(
            (route_key.to_string(), symbol.to_string()),
            (Instant::now(), admission.clone()),
        );
        Ok(admission)
    }

    /// Bind "is a strategy mounted on this `(venue, symbol)`?" (once, by the root). The mount's feed holds that
    /// lane and the gatewayhub allows one tick pump per symbol per session, so a mounted symbol gets no hold: it
    /// is priced already.
    pub fn bind_mounted(&self, mounted: impl Fn(&str, &str) -> bool + Send + Sync + 'static) {
        let _ = self.mounted.set(Box::new(mounted));
    }

    /// **Switch the tap's MOUNT mark to exactly `wanted`** (decision 0245): the `(venue, symbol)` of every published
    /// mount whose engine is on a LABELLED account, boot or runtime ([`crate::readmit::mount_mark_pairs`], handed
    /// by the lane keeper each tick). The core gives a venue's quote, trade and book to its DEFAULT engine only
    /// (decision 0208), so such a mount had a price board written at its bar closes and nothing between them: the
    /// 30-second risk clock skipped it as stale. The tap now hands the core a throttled MARK for the symbol
    /// ([`StrategyLaneCexWatcher::set_mount_mark`]), the lane twin's way, except while the venue itself is stating a mark for
    /// it (a perpetual's kline lane carries one). A pair that left `wanted` (the mount is gone) is switched off; a
    /// venue this node wired no feed for has nothing to switch. Idempotent.
    pub fn sync_mount_marks(&self, wanted: HashSet<(String, String)>) {
        let mut applied = self.mount_marks.lock().unwrap_or_else(PoisonError::into_inner);
        for (venue, symbol) in applied.difference(&wanted) {
            if let Ok(watcher) = self.watcher(venue) {
                watcher.set_mount_mark(venue, symbol, false);
            }
        }
        for (venue, symbol) in wanted.difference(&applied) {
            if let Ok(watcher) = self.watcher(venue) {
                watcher.set_mount_mark(venue, symbol, true);
            }
        }
        *applied = wanted;
    }

    /// **The route key of the running `demo` lease that is `venue`'s DEMO LOGIN**: the session the gatewayhub serves the
    /// venue's quotes off for a paper account (`crate::feeds::paper_quotes_through_demo`; the owner's ruling of
    /// 2026-11-16), picked by whose login the stream rides ([`pick_demo_login`]). `None`: that account does not run here
    /// (a stopped engine's lease is forgotten, [`Self::engine_stopped`]).
    fn demo_login_key(&self, venue: &str) -> Option<String> {
        self.demo_lease_key(crate::feeds::paper_price_login(venue), venue)
    }

    /// The ONE search of the open leases for `venue`'s running `demo` lease, by the `login` rule ([`pick_demo_login`]):
    /// [`Self::demo_login_key`] (a paper account's quotes, the stream's own rule) and [`Self::state_paper_units`] (an
    /// epic's contract size, any demo lease of the venue) both ask it.
    fn demo_lease_key(
        &self,
        login: Option<CredentialedStreamLogin>,
        venue: &str,
    ) -> Option<String> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let leases = sessions.iter().map(|(route_key, session)| {
            let account = session.account();
            (route_key.as_str(), account.venue.as_str(), account.tier)
        });
        pick_demo_login(login, venue, leases)
    }

    /// **The tick size a price lane of `symbol` subscribes on, for `route_key`'s engine**: the symbol facts of the lease
    /// that answers for it ([`Self::lease_key`]: its own, or a demo login's for a `paper` engine of a venue priced
    /// through one or whose paper units come from one, ig). An account with NO lease at all (a `paper` engine of every other venue: it never logs in) reads the
    /// venue's KEPT instrument list row instead, so it is priced from its venue's live quote like every other account
    /// (the owner, 2026-11-16: "the same for ALL"); a symbol the list does not carry is the lease's refusal, but on ig
    /// ([`vike_paper::unit_stated_over_demo_login`]) it is the refusal that names the missing demo login.
    fn lane_tick(&self, route_key: &str, venue: &str, symbol: &str) -> Result<f64, String> {
        let key = self.lease_key(route_key, venue);
        let leased =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).contains_key(&key);
        if !leased && let Some(tick) = kept_tick(venue, symbol) {
            return Ok(tick);
        }
        // ig's tick is read over a demo login only: the datahub's keyless facts would only say no public catalog exists.
        if !leased && vike_paper::unit_stated_over_demo_login(venue) {
            return Err(format!(
                "no {venue} demo login is running, so {} has no tick for `{symbol}`: {venue} publishes none publicly \
                 and its kept list states none. Arm a {venue} demo account (`account.active = 1` on its `demo` row)",
                route_key.replacen('#', "/", 1)
            ));
        }
        self.admission(&key, venue, symbol)
            .map_err(|e| e.text().to_string())?
            .grid
            .tick_size
            .ok_or_else(|| format!("{venue} publishes no tick size for `{symbol}`"))
    }

    /// The route key whose lease answers a facts look for `route_key`'s symbol: its own, or, for an account with no
    /// lease of its own on a venue whose paper prices come through the demo login (a `paper` engine there), the demo
    /// login's ([`Self::demo_login_key`]), and on a venue whose paper units come from the demo login
    /// ([`vike_paper::unit_stated_over_demo_login`]: ig, whose kept list states no grid and whose facts no public catalog answers),
    /// the demo lease [`Self::state_paper_units`] reads them over. The paper account itself never logs in and sends the
    /// venue nothing.
    fn lease_key(&self, route_key: &str, venue: &str) -> String {
        let own =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).contains_key(route_key);
        let demo = if own {
            None
        } else if crate::feeds::paper_quotes_through_demo(venue) {
            self.demo_login_key(venue)
        } else if vike_paper::unit_stated_over_demo_login(venue) {
            self.demo_lease_key(Some(CredentialedStreamLogin::AnyDemoSession), venue)
        } else {
            None
        };
        demo.unwrap_or_else(|| route_key.to_string())
    }

    /// **Price and fill the PAPER engines of the venues priced through their demo login** (the owner's ruling of
    /// 2026-11-16; `engines` is [`crate::readmit::paper_quote_engines`], handed by the lane keeper each tick): the tap
    /// folds each one's own symbol's quotes into its book's bars ([`StrategyLaneCexWatcher::set_paper_bars`], switched
    /// to exactly these pairs), and an engine whose venue has NO running demo login is said on every frame as
    /// [`crate::server::symbol_admission::no_demo_login`] (`trade health`'s engine row reads it), and the statement
    /// goes when a demo login runs or the engine leaves. While the venue's market is closed at `now_ms`
    /// (`crate::readmit::paper_waits_for_open`: its demo login does not log in before the open) the statement is the
    /// wait for the open instead ([`crate::server::symbol_admission::paper_market_closed`]). Idempotent.
    pub fn sync_paper_quotes(&self, engines: &[crate::readmit::ReadmitTarget], now_ms: i64) {
        let wanted: HashSet<(String, String)> =
            engines.iter().map(|t| (t.venue.clone(), t.symbol.clone())).collect();
        let waits = |t: &crate::readmit::ReadmitTarget| {
            crate::readmit::paper_waits_for_open(
                Some(vike_model::AccountTier::Paper),
                &t.venue,
                now_ms,
            )
        };
        let unpriced: HashSet<String> = engines
            .iter()
            .filter(|t| waits(t).is_some() || self.demo_login_key(&t.venue).is_none())
            .map(|t| t.route_key.clone())
            .collect();
        let mut folded = self.paper_bars.lock().unwrap_or_else(PoisonError::into_inner);
        for (venue, symbol) in folded.difference(&wanted) {
            if let Ok(watcher) = self.watcher(venue) {
                watcher.set_paper_bars(venue, symbol, false);
            }
        }
        for (venue, symbol) in wanted.difference(&folded) {
            if let Ok(watcher) = self.watcher(venue) {
                watcher.set_paper_bars(venue, symbol, true);
            }
        }
        *folded = wanted;
        drop(folded);
        let mut said = self.paper_unpriced.lock().unwrap_or_else(PoisonError::into_inner);
        for route_key in said.difference(&unpriced) {
            self.set_venue_link_down(route_key, None);
        }
        for target in engines.iter().filter(|t| unpriced.contains(&t.route_key)) {
            let (why, effect) = match waits(target) {
                Some(open) => (
                    crate::server::symbol_admission::paper_market_closed(&target.venue, open),
                    "this paper account's orders are refused until the open".to_string(),
                ),
                None => (
                    crate::server::symbol_admission::no_demo_login(&target.venue),
                    format!(
                        "this paper account's orders are refused: {}",
                        crate::server::symbol_admission::demo_login_needed(&target.venue)
                    ),
                ),
            };
            self.set_venue_link_down(
                &target.route_key,
                Some(VenueLinkDownWire {
                    route_key: target.route_key.clone(),
                    why,
                    failed_restarts: 0,
                    retry_in_ms: 0,
                    effect,
                    no_adapter: false,
                }),
            );
        }
        *said = unpriced;
    }

    /// Bind the bar series the boot's mounts subscribed (once, by the root): [`Self::keep_bars`] leaves them alone.
    pub fn bind_boot_bars(&self, series: HashSet<SeriesKey>) {
        let _ = self.boot_bars.set(series);
    }

    /// **Open the BAR lane of every mount the boot did not wire** (decision 0245: a mount works by candles and by
    /// ticks): `wanted` is the `(venue, symbol, interval)` of every published mount. A runtime mount (or one
    /// resurrected from the topology sidecar) starts no feed, so a series nobody subscribed was never heard and its
    /// `on_bar` never ran. Each series is asked ONCE, on a thread of its own (the datahub round trip runs with no
    /// lock held), through the venue's watcher, so the bars reach the core through the sink the mounted arm built;
    /// a refusal (a venue with no bar lane of its own, a feed that is down) is said once at `warn` and asked again
    /// after [`HOLD_RETRY_AFTER`], quietly. An open series stays to the process's end: the feed's teardown releases
    /// it with the mounts' own. Idempotent; with no keeper running ([`Self::spawn_bar_keeper`]) it opens nothing.
    /// `wanted` is remembered as the mounts' series, the only lanes [`SymbolAdmitter::bar_series`] counts.
    pub fn keep_bars(&self, wanted: &[SeriesKey]) {
        *self.mount_bars.lock().unwrap_or_else(PoisonError::into_inner) =
            wanted.iter().cloned().collect();
        self.open_bar_lanes(wanted);
    }

    /// Open the bar lane of each series in `wanted` not heard yet ([`Self::keep_bars`]'s body, and a price alert's).
    fn open_bar_lanes(&self, wanted: &[SeriesKey]) {
        let Some(me) = self.me.get().and_then(std::sync::Weak::upgrade) else { return };
        for series in wanted {
            if self.boot_bars.get().is_some_and(|boot| boot.contains(series)) {
                continue;
            }
            {
                let mut bars = self.bars.lock().unwrap_or_else(PoisonError::into_inner);
                match bars.get(series).copied() {
                    Some(BarHold::Open | BarHold::Opening) => continue,
                    Some(BarHold::Refused { at }) if at.elapsed() < HOLD_RETRY_AFTER => continue,
                    _ => {}
                }
                bars.insert(series.clone(), BarHold::Opening);
            }
            let (opener, owned) = (Arc::clone(&me), series.clone());
            let spawned = std::thread::Builder::new()
                .name("bar-hold".into())
                .spawn(move || opener.open_bars(&owned));
            if let Err(e) = spawned {
                self.bars.lock().unwrap_or_else(PoisonError::into_inner).remove(series);
                tracing::warn!(
                    venue = %series.0, symbol = %series.1, interval = %series.2,
                    "could not start the thread that opens a mount's bar lane ({e}); asked again at the next tick"
                );
            }
        }
    }

    /// Subscribe one series ([`Self::keep_bars`]) and settle its entry.
    fn open_bars(&self, series: &SeriesKey) {
        let (venue, symbol, interval) = series;
        let opened = self.watcher(venue).and_then(|watcher| {
            watcher.subscribe_bars(symbol, interval).map_err(|e| {
                format!("cannot subscribe {venue}'s {interval} bars for `{symbol}`: {e}")
            })
        });
        let mut bars = self.bars.lock().unwrap_or_else(PoisonError::into_inner);
        match opened {
            Ok(()) => {
                bars.insert(series.clone(), BarHold::Open);
                drop(bars);
                tracing::info!(
                    %venue, %symbol, %interval,
                    "opened the bar lane of a mount the boot did not wire (decision 0245): it works by candles and by ticks"
                );
            }
            Err(why) => {
                let first = !matches!(bars.get(series), Some(BarHold::Refused { .. }));
                bars.insert(series.clone(), BarHold::Refused { at: Instant::now() });
                drop(bars);
                if first {
                    tracing::warn!(
                        %venue, %symbol, %interval,
                        "a mount's bar lane could not be opened ({why}): the mount hears no `{interval}` bar \
                         (its ticks, if its tick lane is held, still reach it). Asked again about once a \
                         minute, said once"
                    );
                } else {
                    tracing::debug!(%venue, %symbol, %interval, "the mount's bar lane is still refused ({why})");
                }
            }
        }
    }

    /// Start the thread that calls [`Self::keep_bars`] every `every` until the registry is dropped (it holds only a
    /// `Weak`), waiting out the boot window as [`Self::spawn_lane_keeper`]'s does. It also gives `keep_bars` the
    /// handle it opens lanes with.
    pub fn spawn_bar_keeper(
        self: &Arc<Self>,
        every: Duration,
        wanted: impl Fn() -> Vec<SeriesKey> + Send + 'static,
    ) {
        let _ = self.me.set(Arc::downgrade(self));
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-bars".into()).spawn(move || {
                std::thread::sleep(every);
                loop {
                    let Some(facts) = weak.upgrade() else { return };
                    if facts.boot_open() {
                        drop(facts);
                        std::thread::sleep(BOOT_RECHECK);
                        continue;
                    }
                    let began = Instant::now();
                    facts.keep_bars(&wanted());
                    drop(facts);
                    std::thread::sleep(every.saturating_sub(began.elapsed()));
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the mount bar-lane keeper ({e}); a mount added while the node runs hears the \
                 bar series the boot subscribed, and no other"
            );
        }
    }

    /// Whether `symbol`, the own symbol of `route_key`'s engine on `venue`, may get a held lane at all: the venue
    /// is not in [`NO_HELD_LANE`] (whatever the engine's label), the engine has not stopped, and no strategy mount
    /// prices the symbol already. The engine may be the venue's default one or a labelled one (decision 0198: the
    /// quote the lane brings reaches the default engine's board, and a labelled engine is priced by the mark the
    /// tap is switched to hand the core, [`Self::sync_labelled_mark`]). Silent: each of these is a standing fact.
    fn may_hold(&self, route_key: &str, venue: &str, symbol: &str) -> bool {
        !symbol.is_empty()
            && !NO_HELD_LANE.iter().any(|(name, _)| *name == venue)
            && !self
                .engines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(route_key)
                .is_some_and(|state| state.stopped)
            && !self.mounted.get().is_some_and(|mounted| mounted(venue, symbol))
    }

    /// Claim the opening of the held lane of `route_key`'s own `symbol`: `Some((generation, after_refusal))` when
    /// the caller must open it ([`Self::open_hold`]; the entry is now `Opening`), `None` when there is nothing to
    /// do: the engine may not hold a lane ([`Self::may_hold`]), the lane is open (this sighting is stamped, which
    /// is what keeps it past [`HOLD_GRACE`]), being opened, refused less than [`HOLD_RETRY_AFTER`] ago, or the
    /// reaper is closing that very pump (the keeper asks again at its next tick).
    fn claim_hold(&self, route_key: &str, venue: &str, symbol: &str) -> Option<(u64, bool)> {
        if !self.may_hold(route_key, venue, symbol) {
            return None;
        }
        let key = (route_key.to_string(), symbol.to_string());
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let after_refusal = match held.get(&key).copied() {
            Some(Hold::Open { .. }) => {
                held.insert(key, Hold::Open { seen: Instant::now() });
                return None;
            }
            Some(Hold::Opening { .. }) => return None,
            Some(Hold::Refused { at }) if at.elapsed() < HOLD_RETRY_AFTER => return None,
            Some(Hold::Refused { .. }) => true,
            None => false,
        };
        let generation = self.generation(route_key);
        held.insert(key.clone(), Hold::Opening { generation });
        // The reaper marks a pump `Closing` BEFORE it asks whether anyone holds it ([`Self::close_idle`]), and this
        // entry is in BEFORE the pump is looked at: one of the two sees the other, so a lane is never closed
        // under a hold that has just been claimed.
        let closing = self
            .watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .copied()
            == Some(Sub::Closing);
        if closing {
            held.remove(&key);
            return None;
        }
        Some((generation, after_refusal))
    }

    /// Take back a claim [`Self::claim_hold`] made and nothing opened.
    fn unclaim_hold(&self, route_key: &str, symbol: &str) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (route_key.to_string(), symbol.to_string());
        if matches!(held.get(&key), Some(Hold::Opening { .. })) {
            held.remove(&key);
        }
    }

    /// Open the lane a [`Self::claim_hold`] claimed, and settle the entry: `Open`, or `Refused` with the reason
    /// said ONCE at `warn` (a retry that is refused again is `debug`). The facts look-up and the datahub subscribe
    /// run with no lock held, through the very calls the door and the twin's warm-up use (the same watcher, so
    /// the same venue ceilings: OANDA's stream cap, IG's lane cap, the strategy lane's key limits); a ceiling
    /// hit is a `Refused` entry and nothing else, never an order refused.
    fn open_hold(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        generation: u64,
        after_refusal: bool,
    ) {
        let opened = self.watcher(venue).and_then(|watcher| {
            let tick = self.lane_tick(route_key, venue, symbol)?;
            watcher
                .subscribe_ticks(symbol, tick)
                .map_err(|e| format!("cannot subscribe {venue}'s prices for `{symbol}`: {e}"))
        });
        let key = (route_key.to_string(), symbol.to_string());
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let ours = matches!(held.get(&key), Some(Hold::Opening { generation: g }) if *g == generation)
            && self.generation(route_key) == generation;
        if !ours {
            // Released, or the engine stopped (or restarted) while the lane was opening: nobody keeps it now.
            drop(held);
            if opened.is_ok() {
                self.hand_back(venue, symbol);
            }
            return;
        }
        match opened {
            Ok(()) => {
                held.insert(key, Hold::Open { seen: Instant::now() });
                self.sync_labelled_mark(&held, venue, symbol);
                drop(held);
                tracing::info!(
                    %route_key, %symbol,
                    "keeping the price lane of the engine's own symbol open: the engine holds a position or an \
                     order in it (decision 0197)"
                );
            }
            Err(why) => {
                held.insert(key, Hold::Refused { at: Instant::now() });
                drop(held);
                if after_refusal {
                    tracing::debug!(%route_key, %symbol, "the engine's own symbol still has no price lane ({why})");
                } else if self.watcher(venue).is_err() {
                    // A standing fact, not a fault: this node runs no price feed for the venue (polymarket's
                    // prices come from a mount's market feed), so what the engine holds stays unpriced here.
                    tracing::info!(
                        %route_key, %symbol,
                        "no price lane for the engine's own symbol ({why}): what it holds there stays unpriced \
                         on this node"
                    );
                } else {
                    tracing::warn!(
                        %route_key, %symbol,
                        "the price lane of the engine's own symbol could not be kept open ({why}): what the engine \
                         holds there stays unpriced on this node, and no order is refused for it. Asked again \
                         about once a minute, said once"
                    );
                }
            }
        }
    }

    /// **Keep the price lane of `route_key`'s own `symbol` open while the engine holds a position or a live order
    /// in it** (decision 0197): the lease facts, the datahub subscribe, and the entry that makes the lane the
    /// engine's (not counted under [`MAX_WATCHED`], never closed by the reaper) until [`Self::keep_lanes`] or
    /// [`Self::engine_stopped`] hands it back. Idempotent (an open lane is left alone and its sighting stamped),
    /// best effort (a failure is said once and the keeper tries again), and BLOCKING: run it off the order's thread.
    /// The boot calls it for each restored hold, on a short-lived thread ([`crate::readmit::warm_twins`]).
    pub fn hold_price(&self, route_key: &str, venue: &str, symbol: &str) {
        if let Some((generation, after_refusal)) = self.claim_hold(route_key, venue, symbol) {
            self.open_hold(route_key, venue, symbol, generation, after_refusal);
        }
    }

    /// Hand the held lane of `route_key`'s `symbol` back: forget the entry, and give the pump to the idle reaper
    /// unless something else holds it. Answers whether a lane was open (or opening) for it.
    fn drop_hold(&self, route_key: &str, venue: &str, symbol: &str) -> bool {
        let was = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            let was = held.remove(&(route_key.to_string(), symbol.to_string()));
            self.sync_labelled_mark(&held, venue, symbol);
            was
        };
        let had_lane = matches!(was, Some(Hold::Open { .. } | Hold::Opening { .. }));
        if had_lane {
            self.hand_back(venue, symbol);
        }
        had_lane
    }

    /// Give the pump of `(venue, symbol)` to the idle reaper (`Idle` from now), unless an account admitted the
    /// symbol, a lane twin warmed it or another engine holds it. An entry the door already made is left as it is.
    fn hand_back(&self, venue: &str, symbol: &str) {
        if self.admitted_on_venue(venue, symbol)
            || self.warmed_on_venue(venue, symbol)
            || self.held_on_venue(venue, symbol)
        {
            return;
        }
        self.watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry((venue.to_string(), symbol.to_string()))
            .or_insert_with(|| Sub::Idle(Instant::now()));
    }

    /// Whether ANY engine of `venue` holds the lane of `symbol` open (or is opening it): the pump is then the
    /// engine's, whichever account's refused order left a `watched` entry beside it.
    fn held_on_venue(&self, venue: &str, symbol: &str) -> bool {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).iter().any(
            |((route_key, held_symbol), hold)| {
                held_symbol == symbol
                    && venue_of_route_key(route_key) == venue
                    && matches!(hold, Hold::Open { .. } | Hold::Opening { .. })
            },
        )
    }

    /// **Switch the tap's mark for a LABELLED engine's own `symbol` to what `held` says right now** (decision 0198): on
    /// while a labelled engine of `venue` (a route key that is not the venue's default one) has the lane `Open`, off
    /// otherwise. The core hands a venue's quote, trade and book to the venue's default engine only, so a labelled
    /// engine's own symbol is priced in its engine by a MARK (`PriceTap::set_labelled_mark`), the lane twin's way;
    /// the lane itself is the one subscription every holder shares. Called with the `held` guard in hand at every
    /// change of an entry, so two engines changing at once cannot leave the switch at a stale answer. A venue this
    /// node runs no price feed for has nothing to switch.
    fn sync_labelled_mark(
        &self,
        held: &HashMap<(String, String), Hold>,
        venue: &str,
        symbol: &str,
    ) {
        let Ok(watcher) = self.watcher(venue) else { return };
        let wanted = held.iter().any(|((route_key, held_symbol), hold)| {
            held_symbol == symbol
                && route_key != venue
                && venue_of_route_key(route_key) == venue
                && matches!(hold, Hold::Open { .. })
        });
        watcher.set_labelled_mark(venue, symbol, wanted);
    }

    /// **One tick of the lane keeper**: hold the lane of every `(route key, venue, symbol)` in `wanted` (the engines
    /// the published snapshot shows holding a position or a live order in their own symbol,
    /// [`crate::readmit::own_symbol_holds`]), and hand back the lanes of the ones that are not in it any more
    /// ([`HOLD_GRACE`] after they were last wanted; a lane that was refused is forgotten at once, so the next
    /// holding says it afresh). Nothing blocks: a lane is opened on a thread of its own.
    pub fn keep_lanes(&self, wanted: &[crate::readmit::ReadmitTarget]) {
        for target in wanted {
            SymbolAdmitter::hold_lane(self, &target.route_key, &target.venue, &target.symbol);
        }
        let due: Vec<(String, String)> = self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|((route_key, symbol), hold)| {
                let wanted_still =
                    wanted.iter().any(|t| t.route_key == *route_key && t.symbol == *symbol);
                !wanted_still
                    && match hold {
                        Hold::Open { seen } => seen.elapsed() >= HOLD_GRACE,
                        Hold::Refused { .. } => true,
                        Hold::Opening { .. } => false,
                    }
            })
            .map(|(key, _)| key.clone())
            .collect();
        for (route_key, symbol) in due {
            let venue = venue_of_route_key(&route_key);
            if self.drop_hold(&route_key, venue, &symbol) {
                tracing::info!(
                    %route_key, %symbol,
                    "the engine's own symbol is not held any more (it holds nothing there, or it is a paper account \
                     whose market has closed): its price lane goes back to the idle reaper (decision 0197)"
                );
            }
        }
    }

    /// Start the thread that calls [`Self::keep_lanes`] every `every` until the registry is dropped (it holds only a
    /// `Weak`): `wanted` reads the published engines. It also gives [`SymbolAdmitter::hold_lane`] the handle it opens
    /// lanes with. The ticks keep a fixed rate and wait out the boot window, as [`Self::spawn_rate_refresher`]'s do.
    pub fn spawn_lane_keeper(
        self: &Arc<Self>,
        every: Duration,
        wanted: impl Fn() -> Vec<crate::readmit::ReadmitTarget> + Send + 'static,
    ) {
        let _ = self.me.set(Arc::downgrade(self));
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-lanes".into()).spawn(move || {
                std::thread::sleep(every);
                loop {
                    let Some(facts) = weak.upgrade() else { return };
                    if facts.boot_open() {
                        drop(facts);
                        std::thread::sleep(BOOT_RECHECK);
                        continue;
                    }
                    let began = Instant::now();
                    facts.keep_lanes(&wanted());
                    drop(facts);
                    std::thread::sleep(every.saturating_sub(began.elapsed()));
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the price-lane keeper ({e}); an engine's own symbol is priced only by a \
                 strategy mount, or by the order that asked for it"
            );
        }
    }

    /// Shorten [`ADMITTED_IDLE_GRACE`] (once; for a test that must not wait five minutes).
    pub fn set_admitted_idle_grace(&self, grace: Duration) {
        let _ = self.admitted_idle_grace.set(grace);
    }

    fn admitted_idle_grace(&self) -> Duration {
        self.admitted_idle_grace.get().copied().unwrap_or(ADMITTED_IDLE_GRACE)
    }

    /// **One tick of the admitted-pair reaper** (A39): give back the price pump of an ADMITTED symbol of a venue whose
    /// credentialed row reaps one (`vike_gatewayhub_client::credentialed_limits::reaps_idle_admitted`: fxcm, dukascopy,
    /// ibkr, each with its cap's reason) once every account that admitted it has shown `holdings` with no position and
    /// no live order in it for [`ADMITTED_IDLE_GRACE`], and open it again as soon as one holds something in it.
    ///
    /// - **Unknown is not idle.** An account `holdings` has no block for (an engine that has published nothing yet)
    ///   is skipped and its count of idleness forgotten.
    /// - **The count restarts at an order.** A committing ask at the door ([`SymbolAdmitter::price`]) clears the symbol's
    ///   idle stamp, because the snapshot does not show the order yet.
    /// - **Nothing else is closed under anyone.** A pump a lane twin warmed, an engine's held own-symbol lane or a
    ///   strategy mount prices stays. A pump under an open venue gap goes like any other, and its gap is ended with
    ///   it (A55). Several accounts admitting one symbol share one pump: it goes only when ALL of them are idle.
    /// - **The engine keeps the symbol.** The admission, the engine's slot and the daemon's reconcile reads are
    ///   untouched (giving the slot back is the operator's `unadmit-symbol`, decision 0205, and
    ///   [`Self::settle_releases`] forgets the pair's tables then, this reaper's stamps among them); only the price
    ///   lane goes. The next order on the pair opens it
    ///   through the door's own ask and is held for the first price, or refused `has no price on this node yet`
    ///   once, as a pair never admitted is.
    ///
    /// BLOCKING (the datahub round trips run with no lock held): run it on a thread of its own
    /// ([`Self::spawn_admitted_reaper`]).
    pub fn reap_idle_admitted(&self, holdings: &crate::readmit::ReadmitHoldings) {
        let now = Instant::now();
        let grace = self.admitted_idle_grace();
        let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for (route_key, symbol) in
            self.admitted.lock().unwrap_or_else(PoisonError::into_inner).iter()
        {
            let venue = venue_of_route_key(route_key);
            if holdings.known.contains(route_key)
                && vike_gatewayhub_client::credentialed_limits::reaps_idle_admitted(venue).is_some()
            {
                groups
                    .entry((venue.to_string(), symbol.clone()))
                    .or_default()
                    .push(route_key.clone());
            }
        }
        let (mut to_close, mut to_reopen) = (Vec::new(), Vec::new());
        {
            let mut idle = self.idle_admitted.lock().unwrap_or_else(PoisonError::into_inner);
            // A pair no account admits any more (or one whose engine has gone quiet in the snapshot) has no count.
            idle.retain(|(route_key, symbol), _| {
                let venue = venue_of_route_key(route_key);
                groups
                    .get(&(venue.to_string(), symbol.clone()))
                    .is_some_and(|route_keys| route_keys.contains(route_key))
            });
            let reaped = self.reaped.lock().unwrap_or_else(PoisonError::into_inner);
            for ((venue, symbol), route_keys) in &groups {
                let holds = |route_key: &String| {
                    holdings.held.iter().any(|t| &t.route_key == route_key && &t.symbol == symbol)
                };
                let holder = route_keys.iter().find(|route_key| holds(route_key));
                let mut all_past_grace = true;
                for route_key in route_keys {
                    let key = (route_key.clone(), symbol.clone());
                    if holds(route_key) {
                        idle.remove(&key);
                    } else {
                        let since = *idle.entry(key).or_insert(now);
                        all_past_grace &= now.saturating_duration_since(since) >= grace;
                    }
                }
                let pump = (venue.clone(), symbol.clone());
                match (holder, reaped.get(&pump)) {
                    // The engine holds something in a pair whose pump was given back: open it again (a failed try
                    // waits out `HOLD_RETRY_AFTER`).
                    (Some(holder), Some(tried))
                        if tried.is_none_or(|at| at.elapsed() >= HOLD_RETRY_AFTER) =>
                    {
                        to_reopen.push((holder.clone(), venue.clone(), symbol.clone()));
                    }
                    // Every admitter idle for the grace and the pump still open.
                    (None, None) if all_past_grace => {
                        to_close.push((venue.clone(), symbol.clone()))
                    }
                    _ => {}
                }
            }
        }
        for (venue, symbol) in to_close {
            self.close_admitted_pump(&venue, &symbol);
        }
        for (route_key, venue, symbol) in to_reopen {
            self.reopen_admitted_pump(&route_key, &venue, &symbol);
        }
    }

    /// Give the pump of the admitted `(venue, symbol)` back (blocking), unless something else holds it.
    fn close_admitted_pump(&self, venue: &str, symbol: &str) {
        if self.warmed_on_venue(venue, symbol)
            || self.held_on_venue(venue, symbol)
            || self.mounted.get().is_some_and(|mounted| mounted(venue, symbol))
        {
            return;
        }
        let Ok(watcher) = self.watcher(venue) else { return };
        // A pump under an open venue gap is closed like any other: the close ends the gap (A55,
        // `StrategyLaneCexWatcher::close_ticks`), so the core's link dead-man window for the symbol closes with it.
        // Marked BEFORE the pump goes, so a door ask that lands in between (and re-subscribes) clears the mark.
        let pump = (venue.to_string(), symbol.to_string());
        self.reaped.lock().unwrap_or_else(PoisonError::into_inner).insert(pump.clone(), None);
        if watcher.close_ticks(venue, symbol) {
            tracing::info!(
                %venue, %symbol,
                "no account holds a position or an order in this admitted symbol any more: its price pump goes \
                 back (the admission stays; the next order opens it again)"
            );
        } else {
            self.reaped.lock().unwrap_or_else(PoisonError::into_inner).remove(&pump);
        }
    }

    /// Open the pump of the admitted `(venue, symbol)` again because `route_key`'s engine holds something in it
    /// (blocking): the door's own ask, whose success clears the reaped mark. A failure is said once and retried
    /// after [`HOLD_RETRY_AFTER`].
    fn reopen_admitted_pump(&self, route_key: &str, venue: &str, symbol: &str) {
        match SymbolAdmitter::price(self, route_key, venue, symbol, true) {
            Ok(_) => tracing::info!(
                %route_key, %symbol,
                "the engine holds something in an admitted symbol whose price pump had gone back: the pump is open again"
            ),
            Err(why) => {
                let pump = (venue.to_string(), symbol.to_string());
                let mut reaped = self.reaped.lock().unwrap_or_else(PoisonError::into_inner);
                let first = reaped.get(&pump).is_some_and(Option::is_none);
                reaped.insert(pump, Some(Instant::now()));
                drop(reaped);
                if first {
                    tracing::warn!(
                        %route_key, %symbol,
                        "the engine holds something in an admitted symbol whose price pump had gone back, and it \
                         could not be opened again ({why}); what it holds there stays unpriced until it can. \
                         Asked again about once a minute, said once"
                    );
                } else {
                    tracing::debug!(%route_key, %symbol, "the admitted symbol still has no price pump ({why})");
                }
            }
        }
    }

    /// Start the thread that calls [`Self::reap_idle_admitted`] every `every` until the registry is dropped (it holds
    /// only a `Weak`): `holdings` reads the published engines, and answers `None` while there is no snapshot to
    /// read (nothing is reaped then). The ticks keep a fixed rate and wait out the boot window, as
    /// [`Self::spawn_lane_keeper`]'s do.
    pub fn spawn_admitted_reaper(
        self: &Arc<Self>,
        every: Duration,
        holdings: impl Fn() -> Option<crate::readmit::ReadmitHoldings> + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-admitted".into()).spawn(move || {
                std::thread::sleep(every);
                loop {
                    let Some(facts) = weak.upgrade() else { return };
                    if facts.boot_open() {
                        drop(facts);
                        std::thread::sleep(BOOT_RECHECK);
                        continue;
                    }
                    let began = Instant::now();
                    if let Some(holdings) = holdings() {
                        // First the symbols the engine gave back (decision 0205), so the reaper below never
                        // counts idleness for a pair nobody admits any more.
                        facts.settle_releases(&holdings);
                        // ...and the combo legs: an owed commit asked again, a leg nothing needs forgotten.
                        facts.settle_combo_legs(&holdings);
                        facts.reap_idle_admitted(&holdings);
                    }
                    drop(facts);
                    std::thread::sleep(every.saturating_sub(began.elapsed()));
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "could not start the admitted-pair reaper ({e}); the price pumps of admitted symbols stay open \
                 until their engine stops"
            );
        }
    }
}

// ---- the give-back of an admitted symbol (decision 0205) -------------------------------------------------

impl TradeLeaseRegistry {
    /// **One tick of the give-back settler**: finish the requests the door recorded ([`SymbolAdmitter::release_requested`])
    /// for which the published snapshot now shows the engine no longer lists the symbol, and drop the ones the engine
    /// refused. For each request:
    ///
    /// - **The engine's block is unknown** (it published nothing yet): kept, and given up after
    ///   [`RELEASE_RETRY_FOR`].
    /// - **The engine still lists the symbol**: kept for [`RELEASE_WAIT`] (the snapshot lags the core), then dropped:
    ///   the engine refused it (a position or an order in it, a declared leg), its note says why, and this node
    ///   forgets nothing.
    /// - **The engine let go**: [`Self::forget_admitted`] completes it, and a gatewayhub that did not answer leaves
    ///   the request to the next tick until [`RELEASE_RETRY_FOR`] has passed.
    ///
    /// A request stays IN the table through its slow part, so a re-admission of the symbol at any moment before the
    /// answer (`SymbolAdmitter::admitted` drops the entry) is seen after it: [`Self::forget_admitted`] then forgets
    /// nothing and tells the gatewayhub again, whichever of the forget and the admission's commit it took last.
    /// BLOCKING (the gatewayhub round trip runs with no lock held): run it on a thread of its own, the admitted-pair
    /// reaper's ([`Self::spawn_admitted_reaper`]).
    pub fn settle_releases(&self, holdings: &crate::readmit::ReadmitHoldings) {
        let pending: Vec<((String, String), Instant)> = self
            .releasing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, asked)| (key.clone(), *asked))
            .collect();
        for (key, asked) in pending {
            let (route_key, symbol) = (&key.0, &key.1);
            let venue = venue_of_route_key(route_key);
            let drop_request = |why: &str| {
                self.releasing.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
                tracing::info!(%route_key, %symbol, "give-back of an admitted symbol: {why}");
            };
            if !holdings.known.contains(route_key) {
                if asked.elapsed() >= RELEASE_RETRY_FOR {
                    drop_request(
                        "the engine published nothing in all that time; the request is dropped",
                    );
                }
                continue;
            }
            if holdings.listed.contains(&key) {
                if asked.elapsed() >= RELEASE_WAIT {
                    drop_request(
                        "the engine still lists the symbol, so it refused the give-back (its note in the recent \
                         events says why); this node forgets nothing",
                    );
                }
                continue;
            }
            // A re-admission since the table was read has removed it, and then there is nothing to forget.
            if !self.releasing.lock().unwrap_or_else(PoisonError::into_inner).contains_key(&key) {
                continue;
            }
            match self.forget_admitted(route_key, venue, symbol) {
                Ok(true) => tracing::info!(
                    %route_key, %symbol,
                    "the engine gave an admitted symbol back: this node forgot its facts, its price lane and the \
                     gatewayhub's reconcile read, and the slot is free"
                ),
                Ok(false) => tracing::info!(
                    %route_key, %symbol,
                    "an admitted symbol was admitted again while it was being given back: this node keeps it and \
                     told the gatewayhub again"
                ),
                Err(why) if asked.elapsed() >= RELEASE_RETRY_FOR => {
                    self.releasing.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
                    tracing::warn!(
                        %route_key, %symbol,
                        "the engine gave an admitted symbol back, but the gatewayhub was not told ({why}) in \
                         {RELEASE_RETRY_FOR:?}: its reconcile client keeps reading it, and keeps its place in the list, \
                         until the account's engine stops"
                    );
                }
                Err(why) => {
                    tracing::debug!(%route_key, %symbol, "give-back not finished yet ({why}); asked again");
                }
            }
        }
    }

    /// **Forget everything this registry kept for an admitted symbol the engine GAVE BACK**, and tell the gatewayhub's
    /// reconcile client to stop reading it (`TradeLease::forget_symbol`): the symbol leaves the admitted set, the
    /// cached facts, the margin figure, the stamps of the admitted-pair reaper and the rate heal, and its price pump
    /// goes to the idle reaper unless another account admitted the symbol, a lane twin warmed it, an engine holds
    /// its lane or a strategy mount prices it. The twin of what [`Self::engine_stopped`] does for a whole account.
    ///
    /// `Err` only for a gatewayhub that did not answer in time (the caller asks again, and the request stays): nothing
    /// node-side is forgotten then, so the node's mirror of the daemon's list keeps claiming what the daemon may still
    /// read. A daemon that does not know the verb (an older gatewayhub) is not retried: its list keeps the symbol, the
    /// node's mirror too, and the rest is forgotten here.
    ///
    /// `Ok(false)`: the symbol was admitted again during the round trip (the request is gone from the table and the
    /// set holds it), so NOTHING is forgotten: the engine holds the symbol, and with the set emptied no later order
    /// would tell the daemon again and the idle reaper would close its pump. The daemon may have taken the forget after
    /// that admission's commit, so it is committed once more, and its FIFO ends on the admission. A request gone with
    /// the symbol out of the set is a stopped engine ([`Self::engine_stopped`] forgot both): nothing to tell.
    fn forget_admitted(&self, route_key: &str, venue: &str, symbol: &str) -> Result<bool, String> {
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned();
        let told = session.map(|session| {
            self.ask(route_key, |bound| match bound {
                Some(bound) => session.forget_symbol_within(symbol, bound),
                None => session.forget_symbol(symbol),
            })
        });
        let unanswered = match told {
            Some(Err(e)) if e.is_transient() => Some(e.text().to_string()),
            Some(Err(e)) => {
                let why = e.text();
                tracing::warn!(
                    %route_key, %symbol,
                    "the gatewayhub did not forget the symbol ({why}); its reconcile client keeps a place for it"
                );
                None
            }
            Some(Ok(())) | None => None,
        };
        let key = (route_key.to_string(), symbol.to_string());
        // One guard across the request and the set: `SymbolAdmitter::admitted` drops the request BEFORE it inserts, so an
        // admission this reads past inserts after this removal, and stands.
        let readmitted = {
            let mut admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
            let mut releasing = self.releasing.lock().unwrap_or_else(PoisonError::into_inner);
            if !releasing.contains_key(&key) {
                Some(admitted.contains(&key))
            } else {
                if let Some(why) = unanswered {
                    return Err(why);
                }
                releasing.remove(&key);
                admitted.remove(&key);
                None
            }
        };
        if let Some(holds) = readmitted {
            if holds && let Some(Err(why)) = self.commit_to_daemon(route_key, symbol) {
                tracing::warn!(
                    %route_key, %symbol,
                    "the gatewayhub was not told this symbol is admitted again ({why}); its reconcile client may \
                     not read it until the node's next start"
                );
            }
            return Ok(false);
        }
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.leverage.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.failed.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.idle_admitted.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.owed_backoff.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        if let Some(rows) =
            self.journaled.lock().unwrap_or_else(PoisonError::into_inner).get_mut(route_key)
        {
            rows.retain(|(s, _)| s != symbol);
        }
        // Nobody admits the pair now: its pump (open, or already given back by the admitted-pair reaper) is the
        // idle reaper's, exactly as a stopped engine's symbols are, and the reaper's mark for it goes.
        if !self.admitted_on_venue(venue, symbol)
            && !self.mounted.get().is_some_and(|mounted| mounted(venue, symbol))
        {
            let pump = (venue.to_string(), symbol.to_string());
            self.reaped.lock().unwrap_or_else(PoisonError::into_inner).remove(&pump);
            self.watched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(pump)
                .or_insert_with(|| Sub::Idle(Instant::now()));
        }
        Ok(true)
    }
}

#[path = "trade_lease_registry_tests.rs"]
#[cfg(test)]
mod trade_lease_registry_tests;

#[path = "trade_lease_registry_gap_tests.rs"]
#[cfg(test)]
mod trade_lease_registry_gap_tests;

#[path = "trade_lease_registry_combo_legs_tests.rs"]
#[cfg(test)]
mod trade_lease_registry_combo_legs_tests;
