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
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_exec::SymbolAdmission;
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
}

/// "Does this account (by route key) run with an account-wide exposure ceiling?"
type ExposureCap = Box<dyn Fn(&str) -> bool + Send + Sync>;

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

    /// Tell the daemon behind `route_key`'s lease that `symbol` is admitted (`TradeSession::admit_symbol`),
    /// one round trip with no lock held, at the bound of the boot window or the thread's retry round.
    /// `None`: the account has no open lease.
    fn commit_to_daemon(&self, route_key: &str, symbol: &str) -> Option<Result<(), String>> {
        let session =
            self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(route_key).cloned()?;
        Some(
            self.ask(route_key, |bound| match bound {
                Some(bound) => session.admit_symbol_within(symbol, bound),
                None => session.admit_symbol(symbol),
            })
            .map(|_| ())
            .map_err(|e| e.text().to_string()),
        )
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
        let admission = SymbolAdmission {
            route_key: route_key.to_string(),
            symbol: symbol.to_string(),
            grid: SymbolGrid::from_properties(&facts.properties),
            contract_value: facts.contract_value,
            initial_mark: None,
        };
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
            .filter(|(at, _)| at.elapsed() < FACTS_TTL)
            .map(|(_, admission)| admission.clone());
        // A cached answer stands in for the daemon unless it is an ADMITTED symbol the daemon's reconcile client was not
        // told: after a gatewayhub restart (or a re-teaching it refused) nobody told
        // the new one, and the cache must not keep the request from reaching it.
        if let Some(admission) = cached
            && !self.daemon_missed(route_key, symbol)
        {
            return Ok(admission);
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
        // its sixteen slots. One request per admission and per facts refresh, with no lock held;
        // a failure is a warning, and the symbol is then read only after the next commit.
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
        match self.commit_to_daemon(route_key, symbol) {
            Some(told) => told,
            None => Err(format!("`{route_key}` has no open trade lease to tell `{symbol}` over")),
        }
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
        let due = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .is_none_or(|(at, _)| at.elapsed() >= REFRESH_AFTER);
        if !due {
            return None;
        }
        match self.lookup(route_key, venue, symbol) {
            Ok(fresh) => Some(fresh),
            Err(why) => {
                // Try again in a minute, not on every order while the lookup is failing.
                if let Some(entry) =
                    self.answers.lock().unwrap_or_else(PoisonError::into_inner).get_mut(&key)
                    && let Some(back) = Instant::now().checked_sub(REFRESH_AFTER - RETRY_AFTER)
                {
                    entry.0 = back;
                }
                tracing::warn!(
                    %route_key, %symbol,
                    "could not refresh an admitted symbol's facts ({why}); the engine keeps its grid"
                );
                None
            }
        }
    }
}

#[path = "facts_tests.rs"]
#[cfg(test)]
mod facts_tests;

#[path = "facts_gap_tests.rs"]
#[cfg(test)]
mod facts_gap_tests;
