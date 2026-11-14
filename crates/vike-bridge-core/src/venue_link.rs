//! **An account's path to its venue, as its bridge last said it**: the ONE reading behind `trade health`'s
//! `gateway-link` row ([`VenueLinkState`]), and the one cell a bridge that keeps its own session writes it into
//! ([`VenueLink`], found by the account's route key through [`venue_link`]).
//!
//! The lease to the gatewayhub stays up through a venue path that never opened, is reconnecting or was lost, and the
//! engine stays `RUNNING`, so nothing else can say so. The gatewayhub's status verb reads the state and states it in
//! the wire's words. It is a reading: it changes no order's fate, no login, no reconnect and no retry.
//!
//! Every producer answers in this one vocabulary, whatever cell it keeps: a bridge with one session writes a
//! [`VenueLink`]; the shared user-data pump keeps its per-pump map and answers
//! (`crate::websocket_session::user::stream_link::VenueLinkStream::venue_link_state`); a bridge whose path is two paths, or one per trading
//! account, or one per process keeps its own and answers too. The venue's words (`why`, `effect`) and its freshness
//! bound stay its bridge's, passed in.
//!
//! **Nothing known yet is [`VenueLinkState::Unknown`]**, for every venue (ruled U-6): a cell nobody wrote, a word
//! reset by a new start, and an "up" older than the bound its writer gave it. Unknown is never up.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Why a venue path is not working now. Words and counters, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VenueLinkDown {
    /// Why, in the bridge's own words.
    pub why: String,
    /// What this costs the account's orders, in the bridge's own words. `""` = the path carries the orders and the
    /// bridge refuses them until it is back.
    pub effect: &'static str,
    /// The restarts, re-logins or opens the bridge tried since the loss that failed. `0` = none, or not counted.
    pub failed_restarts: u32,
    /// The wait before the bridge's next try, as of the reading. `ZERO` = not said: trying now, or nothing tries.
    pub retry_in: Duration,
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

/// The last word, when it was said, and how long it vouches for the path if it says [`VenueLinkState::Up`].
#[derive(Debug, Clone)]
struct Word {
    said: Instant,
    state: VenueLinkState,
    fresh: Option<Duration>,
}

/// **One account's cell**: its bridge writes the path's state ([`Self::set`]) and the gatewayhub's status verb reads it
/// ([`Self::state`]). Clones share the cell.
#[derive(Debug, Clone, Default)]
pub struct VenueLink(Arc<Mutex<Option<Word>>>);

impl VenueLink {
    /// The account's path now.
    #[must_use]
    pub fn state(&self) -> VenueLinkState {
        self.state_at(Instant::now())
    }

    fn state_at(&self, now: Instant) -> VenueLinkState {
        match self.0.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            // A lost path stays lost until a word says otherwise; only an "up" needs to be recent, because the writer
            // that said it may be stuck or gone.
            Some(Word { said, state: VenueLinkState::Up, fresh: Some(bound) })
                if now.saturating_duration_since(said) > bound =>
            {
                VenueLinkState::Unknown
            }
            Some(word) => word.state,
            None => VenueLinkState::Unknown,
        }
    }

    /// Say the path's state now. `fresh`: how long a [`VenueLinkState::Up`] vouches for the path before it reads
    /// [`VenueLinkState::Unknown`] (the writer's own step, a venue fact); `None` = until the next word. A
    /// [`VenueLinkState::Down`] or [`VenueLinkState::Unknown`] never ages.
    pub fn set(&self, state: VenueLinkState, fresh: Option<Duration>) {
        self.set_at(state, fresh, Instant::now());
    }

    /// [`Self::set`], said at `now`: for a writer that steps on a clock of its own.
    pub fn set_at(&self, state: VenueLinkState, fresh: Option<Duration>, now: Instant) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Word { said: now, state, fresh });
    }

    /// Forget the last word: a new client is starting for the account, and what the last one said is history.
    pub fn reset(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
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

/// **A venue session the gatewayhub SUPERVISES, as its bridge says to probe it** (decision 0164 verdict 7(b)): the venue's
/// half of the daemon's one link watch. The daemon owns the loop, the account rows it re-reads and the rebuild; the
/// bridge owns what is probed and its cadence, so a venue fact never sits in the daemon. Only a bridge whose session
/// cannot heal itself has one (IBKR's socket gateway: `vike_ibkr::gateway_watch::IbkrGateway`).
pub trait VenueLinkProbe: Send + Sync + std::fmt::Debug {
    /// Whether the venue session answers now (one bounded probe); `Err` says why not, never with a credential.
    ///
    /// # Errors
    /// Why the session does not answer.
    fn reachable(&self) -> Result<(), String>;
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
