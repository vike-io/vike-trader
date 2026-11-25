//! **An account's path to its venue, as its bridge last said it**: the ONE reading behind `trade health`'s
//! `gateway-link` row ([`VenueLinkState`]), and the one cell a bridge that keeps its own session writes it into
//! ([`VenueLink`], found by the account's route key through [`venue_link`]).
//!
//! The lease to the gatewayhub stays up through a venue path that never opened, is reconnecting or was lost, and the
//! engine stays `RUNNING`, so nothing else can say so. The gatewayhub's status verb reads the state and states it in
//! the wire's words. It is a reading: it changes no order's fate, no login, no reconnect and no retry.
//!
//! Every producer answers in this one vocabulary, into this one cell: a bridge with one session writes the
//! [`VenueLink`]'s own word; the shared user-data pump writes one sub-path per pump
//! (`crate::websocket_session::user::stream_link`); a bridge whose path is two paths, or one per trading account,
//! writes one sub-path each. The venue's words (`why`, `effect`) and its freshness bound stay its bridge's, passed in.
//!
//! **Nothing known yet is [`VenueLinkState::Unknown`]**, for every venue (ruled U-6): a cell nobody wrote, a word
//! reset by a new start, and an "up" older than the bound its writer gave it. Unknown is never up.
//!
//! The reading states its verdict in the one connection-state vocabulary every market feed reports in
//! ([`VenueLinkState::connection_state`]): a reopen with no failure yet is `Connecting`, a failure is `Error`. A bridge
//! whose path goes down for a real reason says so through [`VenueLinkDown::failed`], whether it counts restarts or not.
//!
//! **A path that is several paths writes NAMED SUB-PATHS of the one cell** ([`VenueLink::claim_path`]: an order session
//! and a trade stream, one path per trading account, one per pump), and the cell reads the WORST of them: any loss is
//! the reading, a path that gave up ([`VenueLinkPath::gave_up`]) before a failure (`failed_restarts > 0`) before a
//! reopen, the first claimed among equals; else any path not known yet makes it [`VenueLinkState::Unknown`], and it is
//! up only when every path is. A claimed path that has said nothing is not known, so the cell is never up before every
//! path has spoken. A cell with no sub-path reads its own word, as it always did.
//!
//! **A loss's wait counts down**: a [`VenueLinkDown::retry_in`] is the wait as of the word, and every reading serves it
//! less the time since, floored at zero.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_model::feed_status::{DataClientConnectionState, order_path_state};

/// Why a venue path is not working now. Words and counters, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VenueLinkDown {
    /// Why, in the bridge's own words.
    pub why: String,
    /// What this costs the account's orders, in the bridge's own words. `""` = the path carries the orders and the
    /// bridge refuses them until it is back.
    pub effect: &'static str,
    /// The failures since the path last worked: the restarts, re-logins or opens the bridge tried that failed, and the
    /// loss itself when it was one ([`Self::failed`]). `0` = the venue ended a session and its reopen has not failed yet.
    /// It is what makes the reading `Error` rather than `Connecting` ([`VenueLinkState::connection_state`]).
    pub failed_restarts: u32,
    /// The wait before the bridge's next try, as of the reading. `ZERO` = not said: trying now, or nothing tries.
    pub retry_in: Duration,
}

impl VenueLinkDown {
    /// **A path down for a real reason** (a session lost, a login refused, a process dead, a connection closed to
    /// orders), not a reopen after the venue ended a session: the loss is itself a failure, so the count is at least one
    /// and the reading is `Error`, said at once by the tradehub's path watch, as the shared pump counts a stall. A bridge
    /// that counts no restarts passes `0` and `Duration::ZERO`; one that counts them passes its own count, kept when
    /// higher. The one way to say it, so a bridge that counts nothing never reads as a routine reopen.
    #[must_use]
    pub fn failed(
        why: String,
        effect: &'static str,
        failed_restarts: u32,
        retry_in: Duration,
    ) -> Self {
        Self { why, effect, failed_restarts: failed_restarts.max(1), retry_in }
    }
}

/// What a bridge knows of an account's path to the venue now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueLinkState {
    /// Nothing said yet, or the last word that the path works is older than its writer's bound: not vouched for.
    Unknown,
    /// The path works.
    Up,
    /// The path is lost, or not open yet.
    Down(VenueLinkDown),
}

impl VenueLinkState {
    /// This reading in the one state vocabulary, by the one order-path mapping
    /// (`vike_model::feed_status::order_path_state`, which the tradehub applies to the same facts off the wire).
    #[must_use]
    pub fn connection_state(&self) -> DataClientConnectionState {
        match self {
            Self::Unknown => order_path_state(None, 0),
            Self::Up => order_path_state(Some(true), 0),
            Self::Down(down) => order_path_state(Some(false), down.failed_restarts),
        }
    }
}

/// The last word, when it was said, and how long it vouches for the path if it says [`VenueLinkState::Up`].
#[derive(Debug, Clone)]
struct Word {
    said: Instant,
    state: VenueLinkState,
    fresh: Option<Duration>,
    /// Said by [`VenueLinkPath::gave_up`]: the writer has ended, and the loss outranks every other.
    gave_up: bool,
}

impl Word {
    /// What this word says at `now`: only an "up" needs to be recent, because the writer that said it may be stuck or
    /// gone; a lost path stays lost until a word says otherwise, and its wait before the next try is counted down from
    /// when it was said (floored at zero: a try that is due is no wait).
    fn reading(word: Option<&Word>, now: Instant) -> VenueLinkState {
        match word {
            Some(Word { said, state: VenueLinkState::Up, fresh: Some(bound), .. })
                if now.saturating_duration_since(*said) > *bound =>
            {
                VenueLinkState::Unknown
            }
            Some(Word { said, state: VenueLinkState::Down(down), .. }) => {
                VenueLinkState::Down(VenueLinkDown {
                    retry_in: down.retry_in.saturating_sub(now.saturating_duration_since(*said)),
                    ..down.clone()
                })
            }
            Some(word) => word.state.clone(),
            None => VenueLinkState::Unknown,
        }
    }
}

/// The cell's last word, and the generation of the writer allowed to change it ([`VenueLink::claim`]).
#[derive(Debug, Default)]
struct Cell {
    word: Option<Word>,
    generation: u64,
    /// The named sub-paths ([`VenueLink::claim_path`]), in the order they were first claimed.
    paths: Vec<SubPath>,
    /// Every path claim this cell has handed out, never reset, so a handle from before a [`VenueLink::reset`] can never
    /// match a later claim of the same name.
    path_claims: u64,
}

/// One named sub-path of a cell: its last word and the generation of the writer allowed to change it.
#[derive(Debug)]
struct SubPath {
    name: String,
    word: Option<Word>,
    generation: u64,
}

/// The worst of `words` at `now`, in claim order: the worst loss (a path that gave up, then a failure, then a reopen;
/// the first claimed among equals), else not known if any is, else up.
fn worst<'a>(words: impl Iterator<Item = Option<&'a Word>>, now: Instant) -> VenueLinkState {
    let mut loss: Option<(u8, VenueLinkDown)> = None;
    let mut unknown = false;
    for word in words {
        match Word::reading(word, now) {
            VenueLinkState::Down(down) => {
                let rank = if word.is_some_and(|w| w.gave_up) {
                    2
                } else {
                    u8::from(down.failed_restarts > 0)
                };
                if loss.as_ref().is_none_or(|(worse, _)| rank > *worse) {
                    loss = Some((rank, down));
                }
            }
            VenueLinkState::Unknown => unknown = true,
            VenueLinkState::Up => {}
        }
    }
    match loss {
        Some((_, down)) => VenueLinkState::Down(down),
        None if unknown => VenueLinkState::Unknown,
        None => VenueLinkState::Up,
    }
}

/// **One account's cell**: its bridge writes the path's state ([`Self::set`]) and the gatewayhub's status verb reads it
/// ([`Self::state`]). Clones share the cell, and a clone of a claimed handle writes as that claim.
#[derive(Debug, Clone, Default)]
pub struct VenueLink {
    cell: Arc<Mutex<Cell>>,
    /// The claim this handle writes as (`None`: it never claimed, and every write lands).
    generation: Option<u64>,
}

impl VenueLink {
    /// The account's path now.
    #[must_use]
    pub fn state(&self) -> VenueLinkState {
        self.state_at(Instant::now())
    }

    fn state_at(&self, now: Instant) -> VenueLinkState {
        let cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        if cell.paths.is_empty() {
            return Word::reading(cell.word.as_ref(), now);
        }
        // With sub-paths the cell's own word counts only once said: a bridge that writes paths alone never writes it.
        let own = cell.word.iter().map(Some);
        worst(own.chain(cell.paths.iter().map(|path| path.word.as_ref())), now)
    }

    /// Say the path's state now. `fresh`: how long a [`VenueLinkState::Up`] vouches for the path before it reads
    /// [`VenueLinkState::Unknown`] (the writer's own step, a venue fact); `None` = until the next word. A
    /// [`VenueLinkState::Down`] or [`VenueLinkState::Unknown`] never ages.
    pub fn set(&self, state: VenueLinkState, fresh: Option<Duration>) {
        self.set_at(state, fresh, Instant::now());
    }

    /// [`Self::set`], said at `now`: for a writer that steps on a clock of its own. A handle of an EARLIER claim than
    /// the cell's last ([`Self::claim`]) writes nothing.
    pub fn set_at(&self, state: VenueLinkState, fresh: Option<Duration>, now: Instant) {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        if self.generation.is_some_and(|mine| mine != cell.generation) {
            return;
        }
        cell.word = Some(Word { said: now, state, fresh, gave_up: false });
    }

    /// Forget the last word and every sub-path: a new client is starting for the account, and what the last one said
    /// is history.
    pub fn reset(&self) {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        cell.word = None;
        cell.paths.clear();
    }

    /// **Forget the last word and take the cell for a new client** ([`Self::reset`], plus a generation): the handle
    /// returned writes, and every handle an EARLIER claim returned writes nothing from now on, so a client that outlives
    /// its teardown (an exec thread past its bounded join) cannot speak over the one that replaced it. Readers and
    /// handles that never claimed are unchanged. The sub-paths go too, and an old path's handle then writes nothing.
    #[must_use]
    pub fn claim(&self) -> VenueLink {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        cell.generation += 1;
        cell.word = None;
        cell.paths.clear();
        VenueLink { cell: Arc::clone(&self.cell), generation: Some(cell.generation) }
    }

    /// **Take the sub-path `name` of this cell for a new writer** (the module doc's named sub-paths): it is declared at
    /// once and not known until its writer speaks, so the cell is not up meanwhile; a path claimed before keeps its
    /// place in the order and forgets its last word, and every handle an EARLIER claim of it returned writes nothing.
    #[must_use]
    pub fn claim_path(&self, name: &str) -> VenueLinkPath {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        cell.path_claims += 1;
        let generation = cell.path_claims;
        match cell.paths.iter().position(|path| path.name == name) {
            Some(at) => {
                cell.paths[at].generation = generation;
                cell.paths[at].word = None;
            }
            None => cell.paths.push(SubPath { name: name.to_string(), word: None, generation }),
        }
        VenueLinkPath { cell: Arc::clone(&self.cell), name: name.to_string(), generation }
    }
}

/// **One named sub-path of a [`VenueLink`]** ([`VenueLink::claim_path`]): its writer says the path's state, and the cell
/// reads the worst of its paths. Clones write as the same claim.
#[derive(Debug, Clone)]
pub struct VenueLinkPath {
    cell: Arc<Mutex<Cell>>,
    name: String,
    generation: u64,
}

impl VenueLinkPath {
    /// [`VenueLink::set`], for this path.
    pub fn set(&self, state: VenueLinkState, fresh: Option<Duration>) {
        self.set_at(state, fresh, Instant::now());
    }

    /// [`VenueLink::set_at`], for this path. A handle of an earlier claim, or of a path the cell has dropped, writes
    /// nothing.
    pub fn set_at(&self, state: VenueLinkState, fresh: Option<Duration>, now: Instant) {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(path) = self.mine(&mut cell) {
            path.word = Some(Word { said: now, state, fresh, gave_up: false });
        }
    }

    /// The path is down and its writer has ended; nothing reopens it until the next claim. It outranks every other loss.
    pub fn gave_up(&self, down: VenueLinkDown) {
        self.gave_up_at(down, Instant::now());
    }

    /// [`Self::gave_up`], said at `now`.
    fn gave_up_at(&self, down: VenueLinkDown, now: Instant) {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(path) = self.mine(&mut cell) {
            let state = VenueLinkState::Down(down);
            path.word = Some(Word { said: now, state, fresh: None, gave_up: true });
        }
    }

    /// The path's writer has ended and nothing reopens it here: the path leaves the cell, which reads the paths left.
    pub fn leave(&self) {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        if self.mine(&mut cell).is_some() {
            cell.paths.retain(|path| path.name != self.name);
        }
    }

    /// This handle's path, while its claim is the newest.
    fn mine<'a>(&self, cell: &'a mut Cell) -> Option<&'a mut SubPath> {
        cell.paths
            .iter_mut()
            .find(|path| path.name == self.name && path.generation == self.generation)
    }
}

/// The [`VenueLink`] of the account `route_key` (the gatewayhub's `AdapterStart::route_key`, which the start's
/// `EventSender::route_key` carries): the one cell its bridge writes and its status report reads, because a start hands
/// its host a client and no slot for this.
/// shortcut: one entry per account for the life of the process, bounded by the directory; upgrade if accounts ever churn.
#[must_use]
pub fn venue_link(route_key: &str) -> VenueLink {
    static LINKS: LazyLock<Mutex<HashMap<String, VenueLink>>> = LazyLock::new(Mutex::default);
    let mut links = LINKS.lock().unwrap_or_else(PoisonError::into_inner);
    links.entry(route_key.to_string()).or_default().clone()
}

/// **A venue session the gatewayhub SUPERVISES, as its bridge says to probe it** (decision 0164 verdict 7(b), amended
/// 2026-11-21): the venue's half of the daemon's one link watch, declared by the bridge's own
/// `crate::venue_adapter::VenueAdapter::link_probe`. The daemon owns the loop, the account rows it re-reads, the FX-week
/// hold of a rebuild (by the venue's calendar) and the rebuild itself; the bridge owns what is probed and its cadence, so a
/// venue fact never sits in the daemon. Only a bridge whose session cannot heal itself has one: IBKR's socket gateway
/// (`vike_ibkr::gateway_watch::IbkrGateway`) and FXCM's ForexConnect session once the SDK gave up on it
/// (`vike_fxcm::account::venue_link::FxcmSessionProbe`).
pub trait VenueLinkProbe: Send + Sync + std::fmt::Debug {
    /// Whether the venue answers now (one bounded probe), asked while the link is up (two failures in a row are a loss)
    /// and before every rebuild; `Err` says why not, never with a credential.
    ///
    /// # Errors
    /// Why the session does not answer.
    fn reachable(&self) -> Result<(), String>;
    /// **The venue session's own word that it ENDED for good** (an SDK's final state), asked while the link is up and acted
    /// on at once, like a bridge's own `ExecLink{up:false}`; `None` = it says nothing of the kind. **DEFAULT `None`**: a
    /// probe that only answers [`Self::reachable`].
    fn lost(&self) -> Option<String> {
        None
    }
    /// **The venue session's own word that it is UP now** (an SDK's `Connected`): `Some(true)` up, `Some(false)` not
    /// (yet). Asked after a rebuild: a rebuild counts as healthy, and the daemon's backoff starts over, only once this
    /// said up for [`Self::probation`], so a venue that accepts each login and drops it is backed off. **DEFAULT `None`**: the probe cannot
    /// tell, and a rebuild whose start returned is healthy at once (a start that logs in before it returns).
    fn up(&self) -> Option<bool> {
        None
    }
    /// **How long a rebuilt session must be SEEN up** ([`Self::up`] saying `Some(true)` on every probe, without a break)
    /// before the rebuild is healthy and the daemon's backoff starts over; a loss before that is one more failed
    /// rebuild. Read only when [`Self::up`] can tell. **DEFAULT zero**: one sighting is enough.
    fn probation(&self) -> Duration {
        Duration::ZERO
    }
    /// What a start PARKED behind this link says while the venue does not answer yet (`trade health` reads it).
    /// **DEFAULT `None`**: the daemon's own words, which name no venue.
    fn not_ready_words(&self) -> Option<&'static str> {
        None
    }
    /// How often the link is probed while it is up, and the rows re-read while it is down.
    fn probe_every(&self) -> Duration;
    /// The wait after the first failed restore.
    fn first_wait(&self) -> Duration;
    /// The ceiling the wait between failed restores grows to.
    fn max_wait(&self) -> Duration;
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
