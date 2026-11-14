//! **An account's private stream, as its pump says it is**: the reading behind `trade health`'s `gateway-link` row for
//! every venue whose fills and venue-side cancels come back on [`super::run_user`].
//!
//! The lease to the gatewayhub stays up through a stream that is down, and the engine stays `RUNNING`, so nothing else
//! can say so. The gatewayhub's status verb reads this cell (`crates/vike-gatewayhub/src/trade/adapters/venue_link.rs`'s `pump`),
//! and [`VenueLinkStream::venue_link_state`] answers it in the one reading every venue answers in (`crate::venue_link`), with
//! what each way down costs the orders. It changes no order's fate: the pump reconnects, resyncs and gives up exactly as
//! it did without it.
//!
//! The pump finds its cell through the thread it runs on ([`report_on_this_thread`]), because every venue's pump body
//! calls the loop on the thread [`super::spawn_user`] or [`super::spawn_user_with_resync`] made and only those builders
//! hold the account's `EventSender` in every venue: a fenced pump's own closures hold the fence and no sender. A pump on a thread nobody
//! told (a test driver, a capture tap) writes nothing. ⚠ A venue whose body runs the loop on ANOTHER thread reads
//! [`VenueLinkStreamState::NotRunning`] for ever, so a new venue spawning its own thread calls [`report_on_this_thread`] itself,
//! as hyperliquid does.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_execution::EventSender;

use crate::link_history::VenueLinkRecord;
use crate::venue_link::{VenueLinkDown, VenueLinkState};

/// The longest reason kept: a venue's error text goes onto every observe frame while the stream is down.
const WHY_LIMIT: usize = 240;

/// Why a stream is not open. Words and counters, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueLinkStreamDown {
    /// The session's end, or the last failed open, in the pump's words.
    pub why: String,
    /// The opens that failed since the last one that worked.
    pub failed_opens: u32,
    /// When the pump opens again. `None`: it is opening now, or it has ended.
    pub retry_at: Option<Instant>,
    /// The venue refused the key and the pump ended: nothing reopens it until the adapter is started again.
    pub gave_up: bool,
}

/// An account's streams, read as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueLinkStreamState {
    /// No pump runs for the account: not started yet, or ended with the core.
    NotRunning,
    /// Every pump of the account has its stream open.
    Open,
    /// One is not. A pump that gave up outranks one that is only reopening.
    Down(VenueLinkStreamDown),
}

/// One account's pumps (a per-symbol client has one each, and okx two sockets): by pump, `None` while its stream is open.
#[derive(Debug, Clone, Default)]
pub struct VenueLinkStream(Arc<Mutex<BTreeMap<u64, Option<VenueLinkStreamDown>>>>, Arc<str>);

impl VenueLinkStream {
    /// The link of the account `events` speaks for; `None` on a bare lane, which names no account.
    #[must_use]
    pub fn of(events: &EventSender) -> Option<Self> {
        events.route_key.map(|key| stream_link(&key))
    }

    /// The account's streams now.
    #[must_use]
    pub fn state(&self) -> VenueLinkStreamState {
        let lanes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if lanes.is_empty() {
            return VenueLinkStreamState::NotRunning;
        }
        let down = lanes.values().flatten();
        down.max_by_key(|d| d.gave_up)
            .map_or(VenueLinkStreamState::Open, |d| VenueLinkStreamState::Down(d.clone()))
    }

    /// Forget every pump: a new adapter is starting, and what the last one's pump said on its way out is history.
    pub fn reset(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// The account's streams now, as the one reading every venue answers in (`crate::venue_link`), with what each way
    /// down costs the orders.
    #[must_use]
    pub fn venue_link_state(&self) -> VenueLinkState {
        venue_link_state_of(self.state(), Instant::now())
    }
}

/// These venues send orders on a path of their own, so a stream that is reopening refuses none.
const REOPENING_EFFECT: &str = "orders still go out on their own path; fills and venue-side cancels arrive when the \
                                stream reopens, which the bridge keeps trying by itself, and what the gap holds is read \
                                back";

/// A stream the venue refused the key for: it stays down, and the same key signs the orders.
const GAVE_UP_EFFECT: &str = "fills and venue-side cancels will NOT arrive, and orders signed with this key may be \
                              refused too: fix the key, then set the account's `account.active` off and on";

/// `state` as the venue-link reading at `now`: only an open stream is up, a pump that never said anything is not known
/// (ruled U-6, as for every venue), and the wait is counted from `now`.
fn venue_link_state_of(state: VenueLinkStreamState, now: Instant) -> VenueLinkState {
    match state {
        VenueLinkStreamState::Open => VenueLinkState::Up,
        VenueLinkStreamState::NotRunning => VenueLinkState::Unknown,
        VenueLinkStreamState::Down(VenueLinkStreamDown {
            why,
            failed_opens,
            retry_at,
            gave_up,
        }) => VenueLinkState::Down(VenueLinkDown {
            why,
            effect: if gave_up { GAVE_UP_EFFECT } else { REOPENING_EFFECT },
            failed_restarts: failed_opens,
            retry_in: retry_at.map_or(Duration::ZERO, |at| at.saturating_duration_since(now)),
        }),
    }
}

/// The [`VenueLinkStream`] of the account `route_key`: the one cell its pumps write and its status report reads, because a
/// start hands its host a client and no slot for this (the twin of `crate::venue_link::venue_link`).
/// shortcut: one entry per account for the life of the process, bounded by the directory; upgrade if accounts ever churn.
#[must_use]
pub fn stream_link(route_key: &str) -> VenueLinkStream {
    static LINKS: LazyLock<Mutex<HashMap<String, VenueLinkStream>>> = LazyLock::new(Mutex::default);
    let mut links = LINKS.lock().unwrap_or_else(PoisonError::into_inner);
    links
        .entry(route_key.to_string())
        .or_insert_with(|| VenueLinkStream(Arc::default(), Arc::from(route_key)))
        .clone()
}

thread_local! {
    static THIS_PUMP: RefCell<Option<VenueLinkStream>> = const { RefCell::new(None) };
}

/// Tell the pump on this thread which account's link it writes.
pub fn report_on_this_thread(link: Option<VenueLinkStream>) {
    THIS_PUMP.with_borrow_mut(|cell| *cell = link);
}

/// One pump's place in its account's link; it leaves when the pump ends, unless the pump gave up: that is the reading
/// the operator needs, and [`VenueLinkStream::reset`] ends it.
pub(super) struct VenueLinkStreamLane {
    link: Option<VenueLinkStream>,
    id: u64,
    keep: bool,
    /// The open session, in `crate::link_history`; none while the stream is down. The cell above is what is true now, and
    /// this is what stays: a drop when the pump ends says we ended it.
    session: Option<VenueLinkRecord>,
}

impl VenueLinkStreamLane {
    /// This thread's pump, not open yet.
    pub(super) fn of_this_thread() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let link = THIS_PUMP.with_borrow(Clone::clone);
        let lane =
            Self { link, id: NEXT.fetch_add(1, Ordering::Relaxed), keep: false, session: None };
        lane.write(Some(VenueLinkStreamDown {
            why: "the private stream is opening".to_string(),
            failed_opens: 0,
            retry_at: None,
            gave_up: false,
        }));
        lane
    }

    fn write(&self, state: Option<VenueLinkStreamDown>) {
        if let Some(link) = &self.link {
            link.0.lock().unwrap_or_else(PoisonError::into_inner).insert(self.id, state);
        }
    }

    pub(super) fn open(&mut self) {
        self.write(None);
        if let Some(link) = &self.link {
            let thread = std::thread::current();
            self.session =
                Some(VenueLinkRecord::trade(Some(&*link.1), thread.name().unwrap_or("user-data")));
        }
    }

    /// The stream is down and the pump opens it again after `wait`. A failed open ends no session, so it writes no row.
    pub(super) fn down(&mut self, why: &str, failed_opens: u32, wait: Duration) {
        if let Some(session) = self.session.take() {
            session.ended(why);
        }
        self.write(Some(VenueLinkStreamDown {
            why: why.chars().take(WHY_LIMIT).collect(),
            failed_opens,
            retry_at: Some(Instant::now() + wait),
            gave_up: false,
        }));
    }

    /// The venue refused the key: the pump is ending and nothing opens the stream again.
    pub(super) fn gave_up(&mut self, why: &str, failed_opens: u32) {
        self.keep = true;
        self.write(Some(VenueLinkStreamDown {
            why: why.chars().take(WHY_LIMIT).collect(),
            failed_opens,
            retry_at: None,
            gave_up: true,
        }));
    }
}

impl Drop for VenueLinkStreamLane {
    fn drop(&mut self) {
        if !self.keep
            && let Some(link) = &self.link
        {
            link.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.id);
        }
    }
}

#[path = "stream_link_tests.rs"]
#[cfg(test)]
mod stream_link_tests;
