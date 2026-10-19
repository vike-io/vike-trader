//! `LeaseFacts` — where the control door gets the FACTS and the PRICE of a symbol its engine was not
//! started on (decisions 0173 and 0174): the node asks the gatewayhub over the account's OWN open
//! trade lease, because the node holds no venue connection and no key but the Trade key.
//!
//! The lease answers `TradeUp::SymbolFacts` from the venue's public instrument list
//! (`vike_gatewayhub_client::trade::TradeSession::symbol_facts`). This type is the node's side of it:
//!
//! * **One registry of open leases, by ROUTE KEY**, filled by `RemoteBackend::start_adapter` when the
//!   adapter answers `Live` and shared by every view of the connection (the plan loop's and the
//!   mount's backends are two views of one). An account with no entry has no lease: the answer is
//!   that, by name, never a guess from another account's lease.
//! * **A cache of answers**, keyed `(route key, symbol)`, valid for [`FACTS_TTL`] and only while the daemon's
//!   reconcile client was told every symbol this node admitted (`TradeSession::has_told`, forgotten at a gatewayhub restart): an order does not
//!   cost a round trip to the venue's instrument list. Only a SUCCESS is cached.
//! * **A refresh** ([`SymbolFacts::refresh`]) for a symbol this source admitted: once the answer is
//!   older than [`REFRESH_AFTER`] the next order on the symbol re-reads the facts and the door sends
//!   them as an ordinary `AdmitSymbol` refresh, so a tick or lot the venue changed is applied within
//!   minutes, not for the engine's life.
//! * **The two per-symbol money facts an answer may carry.** The symbol's own LEVERAGE (decision 0182's
//!   per-symbol twin) comes from the daemon's COMMIT only and is kept per `(route key, symbol)`
//!   ([`SymbolFacts::confirmed_leverage`]); a symbol quoted in ANOTHER currency than the account's (decision
//!   0183, phase 1: OANDA's crosses on a demo account) carries its quote currency and a conversion rate
//!   ([`super::quoted::judge`] refuses an answer without a usable, fresh one), which is reused for
//!   [`RATE_FRESH_FOR`] only, re-read by the next order that finds it older, and kept inside the engine's
//!   window for a held position by the timer ([`LeaseFacts::spawn_rate_refresher`]).
//! * **A price for an admitted symbol** ([`SymbolFacts::price`]): the first committing ask subscribes
//!   the symbol's tick pump on its venue's feed dial ([`LeaseFacts::bind_prices`], bound once the
//!   feeds are wired), on the grid the admission carries; every ask reads the last price that pump
//!   delivered. A symbol the door gave up on before admitting is released ([`SymbolFacts::release`]):
//!   its pump stays open for [`IDLE_TTL`] (the operator's retry finds the same subscription), then
//!   [`LeaseFacts::close_idle_pumps`] closes it, and the number of OPEN pumps of symbols nobody
//!   admitted is bounded ([`MAX_WATCHED`]); a full set gives its oldest idle pump up before it
//!   refuses. The pump of an ADMITTED symbol is never closed here.
//! * **The boot window** ([`LeaseFacts::begin_boot`]): while the boot re-admission runs on the boot
//!   thread, every gatewayhub round trip waits at most the boot bound (`config.gatewayhub_connect_wait_ms`,
//!   the bound `RemoteBackend::start_adapter` uses) and a lease that missed it is not asked again until
//!   the window ends. The runtime any-symbol door keeps the lease's own `request_reply`.
//!
//! ⚠ No lock is held across a network call (a lease RPC, a datahub subscribe): a slow lookup must not
//! queue other orders, the 50 ms price polls or a Telegram `/panic` behind it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_exec::{Command, SymbolAdmission};
use vike_gatewayhub_client::trade::{SymbolFactsError, TradeSession};
use vike_model::SymbolGrid;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;

use crate::feeds::CexWatcher;
use crate::server::symbol_admission::SymbolFacts;

/// How long a looked-up admission is reused by [`SymbolFacts::admission`].
pub const FACTS_TTL: Duration = Duration::from_secs(3600);

/// How old an admitted symbol's facts may get before the next order re-reads them.
pub const REFRESH_AFTER: Duration = Duration::from_secs(600);

/// How long a failed refresh waits before the next order tries again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// **How long a conversion rate is reused** (decision 0183, phase 1): an answer that carries one is served,
/// and counts as fresh, for this long and no longer, where the facts around it live [`FACTS_TTL`] and are
/// refreshed every [`REFRESH_AFTER`]. The engine calls a rate stale at 120 s and then denies opening orders,
/// so an order older than this finds its rate re-read BEFORE it is judged (the door's refresh), and the
/// timer ([`LeaseFacts::spawn_rate_refresher`]) keeps a held position's rate inside the window with no order.
pub const RATE_FRESH_FOR: Duration = Duration::from_secs(30);

/// How long a failed rate refresh waits before the next order (or the next tick) tries again.
const RATE_RETRY_AFTER: Duration = Duration::from_secs(10);

/// How often the rate timer re-reads the rate of a held foreign-quoted position (decision 0183, phase 1:
/// about once a minute): two chances inside the engine's 120 s window.
pub const RATE_REFRESH_EVERY: Duration = Duration::from_secs(60);

/// How long a definitive refusal is remembered by [`SymbolFacts::admission`].
const NEGATIVE_TTL: Duration = Duration::from_secs(30);

/// The most price pumps this process holds OPEN for symbols that were not mounted and are not (yet)
/// admitted: a place is held from the subscribe until the pump is closed, not until the door lets go
/// of it. Admitted symbols are capped at the engine (`vike_exec::MAX_EXTRA_SYMBOLS` per account), so a
/// stream of refused symbols cannot hold pumps open without end.
pub const MAX_WATCHED: usize = 32;

/// How long the pump of a symbol the door gave up on stays open before [`LeaseFacts::close_idle_pumps`]
/// closes it: long enough for the operator's retry of a "no price yet" refusal to find it.
pub const IDLE_TTL: Duration = Duration::from_secs(30);

/// How often the reaper thread ([`LeaseFacts::spawn_idle_reaper`]) looks for idle pumps.
const REAP_EVERY: Duration = Duration::from_secs(5);

/// The most refusals [`LeaseFacts`] remembers at once (one per distinct refused name).
const MAX_FAILED: usize = 256;

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

/// **Does an open venue gap stop the reaper closing an idle pump of `venue`?** Only where the core's link
/// dead-man keeps a window for the venue: a pump closed under a gap leaves that window without its `Live`,
/// and it trips after its grace (cancel + HALT). A `SessionBounded` row (oanda, ig) is never in the core's
/// armed set (`AdapterLinkDisclosure::arms`: a `Discloses` plan needs an `Armed` row, a `StratLinkOnly` one
/// an `Inert` row), so the core drops its statuses at `observe` and no window exists to strand. For such a
/// venue the gap must NOT hold the pump: a pair whose stream never connects (the venue's stream cap, a 4xx,
/// a stuck 429) redials every few seconds, keeps its gap open for ever, and would keep one of the
/// [`MAX_WATCHED`] places and its share of the account's new-connection allowance for ever too. Every other
/// venue keeps the rule (an `Armed` row's window is real, and an `Inert` row may be armed for a hub link).
fn gap_holds_pump(venue: &str) -> bool {
    !matches!(
        vike_venues::link_deadman_default(venue),
        vike_venues::LinkDeadMan::SessionBounded { .. }
    )
}

/// The boot re-admission window ([`LeaseFacts::begin_boot`]).
struct Boot {
    /// What one gatewayhub round trip may take.
    bound: Duration,
    /// The route keys whose lease already missed `bound` in this window: not asked again.
    missed: HashSet<String>,
}

/// Ends the boot window when dropped ([`LeaseFacts::begin_boot`]).
pub struct BootWindow<'a>(&'a LeaseFacts);

impl Drop for BootWindow<'_> {
    fn drop(&mut self) {
        *self.0.boot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Ends one retry round's bounded window when dropped ([`LeaseFacts::begin_round`]).
pub struct RoundWindow<'a>(&'a LeaseFacts);

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
    /// Bumped by every start and stop of the account's engine ([`LeaseFacts::generation`]).
    generation: u64,
    /// `Some` once a hot start or stop spoke: whether THAT engine carries an account ceiling (a stopped
    /// one reads `false`, so the door refuses until the next start says otherwise). `None`: the boot's
    /// engine, answered by the boot's rows.
    cap: Option<bool>,
    /// A hot start's engine whose venue balance has not landed yet: the flag its reconcile leg raises
    /// and when the start announced it ([`LeaseFacts::engine_awaiting_balance`]). `None`: nothing waits
    /// (the boot's engines, a `paper` account, a start with no reconcile leg, a stopped account).
    awaiting: Option<(Arc<AtomicBool>, Instant)>,
}

/// "Does this account (by route key) run with an account-wide exposure ceiling?"
type ExposureCap = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// How long a kept answer is served: [`FACTS_TTL`], or [`RATE_FRESH_FOR`] when it carries a conversion rate.
fn kept_for(admission: &SymbolAdmission) -> Duration {
    if admission.quote_rate.is_some() { RATE_FRESH_FOR } else { FACTS_TTL }
}

/// How long a kept answer waits before an order re-reads it: [`REFRESH_AFTER`], or [`RATE_FRESH_FOR`] when
/// it carries a conversion rate.
fn refresh_after(admission: &SymbolAdmission) -> Duration {
    if admission.quote_rate.is_some() { RATE_FRESH_FOR } else { REFRESH_AFTER }
}

/// See the module doc.
#[derive(Default)]
pub struct LeaseFacts {
    sessions: Mutex<HashMap<String, TradeSession>>,
    answers: Mutex<HashMap<(String, String), (Instant, SymbolAdmission)>>,
    /// Each venue's feed handle, bound once the feeds are wired.
    prices: OnceLock<HashMap<String, CexWatcher>>,
    /// `(venue, symbol)` -> the state of its price subscription.
    watched: Mutex<HashMap<(String, String), Sub>>,
    /// `(route key, symbol)` pairs the door (or the boot) sent `AdmitSymbol` for.
    admitted: Mutex<HashSet<(String, String)>>,
    /// `(route key, symbol)` -> the leverage the venue was SEEN holding for the symbol at the last COMMIT
    /// to the daemon (`TradeSession::admit_symbol_confirmed`): set on a figure, removed when the commit
    /// confirmed none. The facts LOOK reads it (`lookup`), so a refresh sent later (every
    /// [`REFRESH_AFTER`]) restates the symbol's margin rate instead of dropping it; only a commit writes
    /// it, because only a commit may ask the exchange anything.
    leverage: Mutex<HashMap<(String, String), f64>>,
    /// `(route key, symbol)` pairs committed OFF the thread that sent the admission
    /// ([`SymbolFacts::admitted_detached`], the Telegram poller's), whose figure the door could therefore not
    /// state ahead of the order. The next [`SymbolFacts::refresh`] of the symbol answers with it at once,
    /// without waiting for [`REFRESH_AFTER`].
    unstated: Mutex<HashSet<(String, String)>>,
    /// Route key -> the currency the VENUE stated for the account when its adapter came up (`AdapterReply::Live`'s
    /// `account_ccy`), which the engine's ledger is anchored to for an account that converts. A symbol quoted in
    /// another currency is admitted only for a route key that has one: every rate the engine is then handed is
    /// expressed in that currency, and an engine whose ledger label is a default nobody agreed to would be
    /// handed rates in a currency it does not know ([`Self::anchor_currency`]).
    anchored: Mutex<HashMap<String, String>>,
    /// `(route key, symbol)` -> when the gatewayhub last REFUSED the symbol (unknown, no tick size, ...), and
    /// why: not asked again for [`NEGATIVE_TTL`] (a flood of orders must not become a flood of lookups).
    /// A link failure or a busy daemon is never entered.
    failed: Mutex<HashMap<(String, String), (Instant, String)>>,
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

/// What [`LeaseFacts::claim_pump`] decided for a symbol the door asks a price of.
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
    failed: &mut HashMap<(String, String), (Instant, String)>,
    key: (String, String),
    why: String,
) {
    if failed.len() >= MAX_FAILED && !failed.contains_key(&key) {
        failed.retain(|_, (at, _)| at.elapsed() < NEGATIVE_TTL);
    }
    if failed.len() < MAX_FAILED || failed.contains_key(&key) {
        failed.insert(key, (Instant::now(), why));
    }
}

impl LeaseFacts {
    /// Tell the registry the bound a boot round trip gets: `config.gatewayhub_connect_wait_ms`, the bound
    /// of `RemoteBackend::start_adapter` (set once, by the backend that was given it).
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
        call: impl FnOnce(Option<Duration>) -> Result<T, SymbolFactsError>,
    ) -> Result<T, SymbolFactsError> {
        let me = std::thread::current().id();
        let window = |boot: &Boot| -> Result<Duration, SymbolFactsError> {
            if boot.missed.contains(route_key) {
                return Err(SymbolFactsError::Unavailable(format!(
                    "boot: `{route_key}`'s trade lease did not answer within {:?} for an earlier \
                     symbol, so it is not asked again before the node is ready",
                    boot.bound
                )));
            }
            Ok(boot.bound)
        };
        let looked: Option<Result<Duration, SymbolFactsError>>;
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
            && matches!(answer, Err(SymbolFactsError::Unavailable(_)))
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
    pub fn register(&self, route_key: &str, session: TradeSession) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(route_key.to_string(), session);
        // A remembered lookup failure was about the OLD link: the new lease asks afresh.
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _), _| key != route_key);
    }

    /// Record the currency the venue stated for `route_key`'s account at its adapter's start (`None`: it stated
    /// none, or the daemon predates the field, and a symbol quoted in another currency is then refused). Called by
    /// `RemoteBackend::start_adapter` beside [`Self::register`], so a later start of the account replaces it.
    pub fn anchor_currency(&self, route_key: &str, currency: Option<&str>) {
        let mut anchored = self.anchored.lock().unwrap_or_else(PoisonError::into_inner);
        match currency.map(str::trim).filter(|c| !c.is_empty()) {
            Some(ccy) => {
                anchored.insert(route_key.to_string(), ccy.to_ascii_uppercase());
            }
            None => {
                anchored.remove(route_key);
            }
        }
    }

    /// Bind each venue's feed handle (once, after the feeds are wired). Until it is, a price ask is
    /// answered `Err`: nothing can deliver one.
    pub fn bind_prices(&self, watchers: HashMap<String, CexWatcher>) {
        let _ = self.prices.set(watchers);
    }

    /// Whether this node ADMITTED `symbol` into `route_key`'s engine but the account's current
    /// daemon was not told (`TradeSession::has_told`): the daemon behind the lease was replaced, or a
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

    /// **The account's engine is about to be (re)built by a hot start** (decision 0169), called BEFORE the
    /// start so the engine is never visible to the door under the previous engine's answer:
    /// `cap_armed` is whether the engine the start builds carries an account-wide ceiling, read from
    /// the rows the start will build it from ([`rows_arm_a_cap`]). The door's
    /// [`SymbolFacts::exposure_cap_armed`] answers this instead of the boot's rows, and the account's
    /// generation moves ([`Self::generation`]). Returns what the answer was, for
    /// [`Self::engine_start_refused`].
    pub fn engine_starting(&self, route_key: &str, cap_armed: bool) -> Option<bool> {
        let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        let state = engines.entry(route_key.to_string()).or_default();
        state.generation += 1;
        state.cap.replace(cap_armed)
    }

    /// **The account's engine was just started in the running node and its venue balance has not landed**
    /// (decision 0169): its equity is the seed cash until the reconcile driver's first pass over its leg, so
    /// [`SymbolFacts::awaiting_balance`] answers `Some` and the door refuses its opening orders until
    /// `synced` is raised. Called by the start BEFORE the engine joins the core (the boot never calls it:
    /// its engines are seeded first by design, so nothing is held there). A later start of the same
    /// account replaces the entry.
    pub fn engine_awaiting_balance(&self, route_key: &str, synced: &Arc<AtomicBool>) {
        let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        engines.entry(route_key.to_string()).or_default().awaiting =
            Some((Arc::clone(synced), Instant::now()));
    }

    /// The start announced by [`Self::engine_starting`] built no engine: the cap answer goes back to
    /// `before` (the generation stays moved, which only ends work tied to the older one).
    pub fn engine_start_refused(&self, route_key: &str, before: Option<bool>) {
        let mut engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        engines.entry(route_key.to_string()).or_default().cap = before;
    }

    /// The price watcher this node holds for `venue`, or the door's words for its absence.
    fn watcher(&self, venue: &str) -> Result<&CexWatcher, String> {
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
    /// [`SymbolFacts::release`] and [`SymbolFacts::price`] treat a symbol the new engine asks for as
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
        }
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(route_key);
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
        self.unstated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _)| key != route_key);
        let venue = route_key.split('#').next().unwrap_or_default();
        for symbol in gone {
            if self.admitted_on_venue(venue, &symbol) {
                continue;
            }
            self.watched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry((venue.to_string(), symbol))
                .or_insert_with(|| Sub::Idle(Instant::now()));
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
            .any(|(route_key, s)| s == symbol && route_key.split('#').next() == Some(venue))
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
    /// account admitted meanwhile frees its place but keeps its pump; one with a venue gap open keeps
    /// both and is tried again at the next call. The datahub round trips run with no lock held.
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
        for (since, key) in picked {
            let (venue, symbol) = &key;
            let watcher = watchers.get(venue);
            let keep_pump = self.admitted_on_venue(venue, symbol);
            let blocked = !keep_pump
                && gap_holds_pump(venue)
                && watcher.is_some_and(|w| w.gap_open(venue, symbol));
            let closed = if blocked || keep_pump {
                false
            } else {
                watcher.is_some_and(|w| w.close_ticks(venue, symbol))
            };
            let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
            // Only the entry this call marked: `admitted` may have removed it meanwhile.
            if watched.get(&key) != Some(&Sub::Closing) {
                continue;
            }
            if blocked {
                watched.insert(key, Sub::Idle(since));
            } else {
                watched.remove(&key);
                freed += 1;
                if closed {
                    tracing::info!(
                        %venue, %symbol,
                        "closed the price subscription of a symbol that was never admitted"
                    );
                }
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
    /// (`TradeSession::admit_symbol_confirmed`), one round trip with no lock held, at the bound of the boot
    /// window or the thread's retry round. `None`: the account has no open lease.
    ///
    /// The commit is the one request that may ask the exchange anything: on a venue that posts a leverage the
    /// daemon makes it hold the account's figure for the symbol and reads it back, and the reply names the
    /// figure it SAW. That figure is kept for [`SymbolFacts::confirmed_leverage`] (set on `Some`, removed on
    /// `None`; a commit that fails says nothing and leaves what is kept).
    ///
    /// ⚠ **The commit takes seconds on a venue that posts a leverage** (three or four signed reads, at most one
    /// write, a poll to see it took), where it used to be one round trip. Three rules keep that off the paths
    /// that cannot afford it: a symbol the daemon was told and the node holds a figure for is NOT committed
    /// again ([`Self::settled`]: a repeat commit on an actively traded symbol is the one that could move the
    /// leverage under an open position); the Telegram poller never waits for it (`FailFast::admitted` runs it
    /// off the poller's thread); and a commit that times out or fails is "unconfirmed" and nothing else: the
    /// symbol stays at the engine's own rate, and the daemon, which keeps going, is read back by the next
    /// commit that runs.
    fn commit_to_daemon(&self, route_key: &str, symbol: &str) -> Option<Result<(), String>> {
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
            Err(e) => Err(e.text().to_string()),
        })
    }

    /// Whether a commit for `symbol` has nothing left to do: the account's CURRENT daemon was told the symbol
    /// (`TradeSession::has_told`) and the venue was SEEN holding a leverage for it ([`Self::leverage`]). The
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
    /// it, and none otherwise: what a facts answer carries out, so a refresh restates the rate.
    fn with_leverage(&self, mut admission: SymbolAdmission) -> SymbolAdmission {
        admission.im_requirement = self
            .leverage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(admission.route_key.clone(), admission.symbol.clone()))
            .map(|x| 1.0 / x);
        admission
    }

    /// The facts of `symbol` over `route_key`'s lease, ignoring the cache.
    fn lookup(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolFactsError> {
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .cloned()
            .ok_or_else(|| {
                SymbolFactsError::Unavailable(format!(
                    "`{route_key}` has no open trade lease at the gatewayhub on this node, so \
                     {venue} cannot be asked about `{symbol}`"
                ))
            })?;
        let facts = self.ask(route_key, |bound| match bound {
            Some(bound) => session.symbol_facts_within(symbol, bound),
            None => session.symbol_facts(symbol),
        })?;
        // THE SYMBOL'S CURRENCY (decision 0183, phase 1). An answer that names none is a symbol quoted in
        // the account's own currency (the daemon's fence is what keeps that true); one that names another
        // is admitted only with a usable, fresh rate, and only for an account the rule lets convert: a
        // refusal here is the symbol NOT admitted, at the boot's re-admission and at the timer as well as
        // at the door, because every one of them asks through this function.
        let quote = match super::quoted::judge(venue, symbol, &facts, vike_model::now_ms()) {
            Ok(quote) => quote,
            Err(super::quoted::Verdict::Refused(why)) => {
                return Err(SymbolFactsError::Refused(why));
            }
            Err(super::quoted::Verdict::Unavailable(why)) => {
                return Err(SymbolFactsError::Unavailable(why));
            }
        };
        if quote.is_some()
            && let Some(why) = crate::server::symbol_admission::quote_conversion_refusal(
                venue,
                Some(session.account().tier),
                self.exposure_cap_armed(route_key),
            )
        {
            return Err(SymbolFactsError::Refused(format!(
                "{venue}: `{symbol}` is quoted in {} and cannot be admitted: {why}",
                quote.as_ref().map_or("another currency", |q| q.ccy.as_str())
            )));
        }
        if let Some(q) = &quote
            && !self.anchored.lock().unwrap_or_else(PoisonError::into_inner).contains_key(route_key)
        {
            return Err(SymbolFactsError::Refused(format!(
                "{venue}: `{symbol}` is quoted in {} and this account's own currency is not known (the \
                 venue's account summary was not read when the account started, or the gatewayhub \
                 predates it), so the rate cannot be tied to a currency the ledger is in. The symbol is \
                 NOT admitted: set the account's `account.active` off and on to start it again",
                q.ccy
            )));
        }
        let admission = SymbolAdmission {
            route_key: route_key.to_string(),
            symbol: symbol.to_string(),
            grid: SymbolGrid::from_properties(&facts.properties),
            contract_value: facts.contract_value,
            initial_mark: None,
            quote_ccy: quote.as_ref().map(|q| q.ccy.clone()),
            quote_rate: quote.as_ref().map(|q| q.rate),
            rate_as_of_ms: quote.as_ref().map(|q| q.as_of_ms),
            // Filled below from the figure a COMMIT confirmed: a look asks the exchange nothing.
            im_requirement: None,
        };
        let admission = self.with_leverage(admission);
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).insert(
            (route_key.to_string(), symbol.to_string()),
            (Instant::now(), admission.clone()),
        );
        Ok(admission)
    }
}

impl SymbolFacts for LeaseFacts {
    fn price_source(&self, venue: &str) -> Result<(), String> {
        self.watcher(venue).map(|_| ())
    }

    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
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
            // Only the daemon's DEFINITIVE answer is remembered; a lost link, a timeout or a busy or
            // restarting daemon says nothing about the symbol, so the next order asks again.
            Err(SymbolFactsError::Refused(why)) => {
                remember_refusal(&mut failed, key, why.clone());
            }
            Err(SymbolFactsError::Unavailable(_)) => {
                failed.remove(&key);
            }
        }
        looked.map_err(|e| e.text().to_string())
    }

    fn exposure_cap_armed(&self, route_key: &str) -> bool {
        let hot = self
            .engines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route_key)
            .and_then(|state| state.cap);
        hot.unwrap_or_else(|| self.exposure.get().is_some_and(|armed| armed(route_key)))
    }

    fn awaiting_balance(&self, route_key: &str) -> Option<Duration> {
        let engines = self.engines.lock().unwrap_or_else(PoisonError::into_inner);
        let (synced, since) = engines.get(route_key)?.awaiting.as_ref()?;
        (!synced.load(Ordering::Acquire)).then(|| since.elapsed())
    }

    fn generation(&self, route_key: &str) -> u64 {
        LeaseFacts::generation(self, route_key)
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
                let opened = self
                    .admission(route_key, venue, symbol)
                    .and_then(|a| {
                        a.grid
                            .tick_size
                            .ok_or_else(|| format!("{venue} publishes no tick size for `{symbol}`"))
                    })
                    .and_then(|tick| {
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
            }
        }
        // A venue whose tap refuses a stream (ibkr: a delayed quote is not a mark) says so here.
        watcher.door_price(venue, symbol)
    }

    fn admitted(&self, route_key: &str, venue: &str, symbol: &str) {
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

    fn admitted_detached(&self, route_key: &str, venue: &str, symbol: &str) {
        self.admitted(route_key, venue, symbol);
        // The order this admission rode has gone out by now at the engine's own rate; what the commit
        // confirmed is stated by the symbol's next refresh.
        if self.confirmed_leverage(route_key, symbol).is_some() {
            self.unstated
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert((route_key.to_string(), symbol.to_string()));
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

    fn release(&self, route_key: &str, venue: &str, symbol: &str) {
        let admitted = self
            .admitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(route_key.to_string(), symbol.to_string()));
        if admitted {
            return;
        }
        // The pump stays open and keeps its place under [`MAX_WATCHED`] for [`IDLE_TTL`], so the
        // operator's retry finds it; [`Self::close_idle_pumps`] closes it after that (never under an
        // open venue gap: the core's link dead-man window would be left without its `Live` and trips
        // after its grace, cancel + HALT).
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sub) = watched.get_mut(&(venue.to_string(), symbol.to_string()))
            && *sub == Sub::Open
        {
            *sub = Sub::Idle(Instant::now());
        }
    }

    fn refresh(&self, route_key: &str, venue: &str, symbol: &str) -> Option<SymbolAdmission> {
        let key = (route_key.to_string(), symbol.to_string());
        if !self.admitted.lock().unwrap_or_else(PoisonError::into_inner).contains(&key) {
            return None;
        }
        // A figure that landed after the admission went out (the commit ran off the sender's thread) is stated
        // NOW, from the facts already kept, instead of at the next due refresh.
        if self.unstated.lock().unwrap_or_else(PoisonError::into_inner).remove(&key)
            && let Some(kept) = self
                .answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .map(|(_, a)| a.clone())
                // A conversion rate is never re-stated from the cache: its own window decides (below).
                .filter(|a| a.quote_rate.is_none())
        {
            return Some(self.with_leverage(kept));
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

impl LeaseFacts {
    /// The facts of an admitted symbol read AGAIN, whatever their age: the body of [`SymbolFacts::refresh`] once
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
    /// ones the rate timer keeps fresh.
    pub fn rated_symbols(&self) -> Vec<(String, String)> {
        let admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let mut rated: Vec<(String, String)> = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(key, (_, a))| a.quote_rate.is_some() && admitted.contains(*key))
            .map(|(key, _)| key.clone())
            .collect();
        rated.sort();
        rated
    }

    /// **One tick of the rate timer** (decision 0183, phase 1): for every admitted symbol this registry holds a
    /// rate for that `held` says the account still has something in (a position or a resting order), read the
    /// venue's rate again and hand the fresh admission to `send`, which puts it on the core's lane. Returns how
    /// many were sent. A symbol that is not held is left alone (the order path re-reads its rate before the next
    /// order is judged); one whose re-read failed keeps the rate the engine has and is tried again at the next
    /// tick, soon ([`RATE_RETRY_AFTER`]). Nothing here is the door: no price is asked and the daemon is not told
    /// anything, the symbol is already admitted.
    pub fn refresh_held_rates(
        &self,
        held: &dyn Fn(&str, &str) -> bool,
        send: &mut dyn FnMut(Command),
    ) -> usize {
        let mut sent = 0;
        for (route_key, symbol) in self.rated_symbols() {
            if !held(&route_key, &symbol) {
                continue;
            }
            let venue = route_key.split('#').next().unwrap_or_default();
            if let Some(fresh) = self.reread(&route_key, venue, &symbol) {
                send(Command::AdmitSymbol(Box::new(fresh)));
                sent += 1;
            }
        }
        sent
    }

    /// Start the thread that calls [`Self::refresh_held_rates`] every `every` until the registry is dropped (it
    /// holds only a `Weak`): the rate of a foreign-quoted position is re-read about once a minute with no order
    /// to carry it (the engine calls a rate stale at 120 s). `held` is asked from this thread and `send` is the
    /// core's lossless lane.
    pub fn spawn_rate_refresher(
        self: &Arc<Self>,
        every: Duration,
        held: impl Fn(&str, &str) -> bool + Send + 'static,
        mut send: impl FnMut(Command) + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        let spawned =
            std::thread::Builder::new().name("lease-facts-rates".into()).spawn(move || {
                loop {
                    std::thread::sleep(every);
                    let Some(facts) = weak.upgrade() else { return };
                    facts.refresh_held_rates(&held, &mut send);
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

#[path = "facts_tests.rs"]
#[cfg(test)]
mod facts_tests;

#[path = "facts_gap_tests.rs"]
#[cfg(test)]
mod facts_gap_tests;
