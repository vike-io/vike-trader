//! **An account's private stream, as its pump says it is**: the reading behind `trade health`'s `gateway-link` row for
//! every venue whose fills and venue-side cancels come back on [`super::run_user`].
//!
//! The lease to the gatewayhub stays up through a stream that is down, and the engine stays `RUNNING`, so nothing else
//! can say so. Each pump writes ONE named sub-path of its account's one link cell (`crate::venue_link::venue_link` by
//! the route key, [`crate::venue_link::VenueLink::claim_path`]), which the gatewayhub's status verb reads for every
//! reporting venue alike (`crates/vike-gatewayhub/src/trade/adapters/venue_link.rs`'s `cell`): the one reading every venue
//! answers in, with what each way down costs the orders, and in the one state vocabulary
//! (`crate::venue_link::VenueLinkState::connection_state`): `Connecting` while a session the venue or the socket ended
//! reopens (or the venue refuses opens its refusal mode still spares), `Error` once a stall, an open or the key failed.
//! The cell reads the worst of an account's pumps: one that gave up, then a failure, then a reopen. It changes no
//! order's fate: the pump reconnects, resyncs and gives up exactly as it did without it.
//!
//! The pump finds its cell through the thread it runs on ([`report_on_this_thread`]), because every venue's pump body
//! calls the loop on the thread [`super::spawn_user`] or [`super::spawn_user_with_resync`] made and only those builders
//! hold the account's `EventSender` in every venue: a fenced pump's own closures hold the fence and no sender. A pump on a thread nobody
//! told (a test driver, a capture tap) writes nothing. ⚠ A venue whose body runs the loop on ANOTHER thread leaves its
//! account's cell unknown for ever, so a new venue spawning its own thread calls [`report_on_this_thread`] itself, as
//! oanda's transactions stream and IBKR's Client Portal pump do.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use vike_execution::EventSender;

use crate::link_history::VenueLinkRecord;
use crate::venue_link::{VenueLinkDown, VenueLinkPath, VenueLinkState, venue_link};

/// The longest reason kept: a venue's error text goes onto every observe frame while the stream is down.
const WHY_LIMIT: usize = 240;

/// **The account a pump on this thread reports for**: its route key, which names the account's link cell
/// (`crate::venue_link::venue_link`) and its rows in `crate::link_history`.
#[derive(Debug, Clone)]
pub struct VenueLinkStream(Arc<str>);

impl VenueLinkStream {
    /// The link of the account `events` speaks for; `None` on a bare lane, which names no account.
    #[must_use]
    pub fn of(events: &EventSender) -> Option<Self> {
        events.route_key.map(|key| stream_link(&key))
    }
}

/// These venues send orders on a path of their own, so a stream that is reopening refuses none.
const REOPENING_EFFECT: &str = "orders still go out on their own path; fills and venue-side cancels arrive when the \
                                stream reopens, which the bridge keeps trying by itself, and what the gap holds is read \
                                back";

/// A stream the venue refused the key for: it stays down, and the same key signs the orders.
const GAVE_UP_EFFECT: &str = "fills and venue-side cancels will NOT arrive, and orders signed with this key may be \
                              refused too: fix the key, then set the account's `account.active` off and on";

/// A pump whose thread panicked: it is gone, and nothing reopens it until the next start.
const PANICKED_WHY: &str = "the private stream's thread panicked (a bug; the log has its message)";
const PANICKED_EFFECT: &str = "fills and venue-side cancels will NOT arrive until the adapter restarts (set the \
                               account's `account.active` off and on); orders still go out on their own path";

/// A pump's replay supervisor whose thread panicked: the stream goes on, and what a reconnect's gap held is not read back.
const REPLAY_PANICKED_PATH: &str = "private stream replay";
pub(super) const REPLAY_PANICKED_WHY: &str =
    "the private stream's replay thread panicked (a bug; the log has its message)";
pub(super) const REPLAY_PANICKED_EFFECT: &str = "fills and venue-side cancels a reconnect's gap held are NOT read back until the \
                                      adapter restarts (set the account's `account.active` off and on); the stream \
                                      itself and orders go on";

/// **The replay supervisor of the account `events` speaks for panicked** (decision 0302: a dead thread is noticed):
/// one `error` line, and a sub-path of the account's cell of its own gives up, so the cell reads failed until the next
/// start resets it. A bare lane (no route key) only says the line.
pub(super) fn replay_panicked(events: &EventSender) {
    tracing::error!(
        target: "vike_bridge_core",
        "{REPLAY_PANICKED_WHY}: a reconnect's gap is no longer read back, and the account's link reads failed"
    );
    if let Some(route_key) = events.route_key {
        let down = VenueLinkDown::failed(
            REPLAY_PANICKED_WHY.to_string(),
            REPLAY_PANICKED_EFFECT,
            0,
            Duration::ZERO,
        );
        venue_link(&route_key).claim_path(REPLAY_PANICKED_PATH).gave_up(down);
    }
}

/// The [`VenueLinkStream`] of the account `route_key`: what a pump is told to report for ([`report_on_this_thread`]),
/// because a start hands its host a client and no slot for this.
#[must_use]
pub fn stream_link(route_key: &str) -> VenueLinkStream {
    VenueLinkStream(Arc::from(route_key))
}

thread_local! {
    static THIS_PUMP: RefCell<Option<VenueLinkStream>> = const { RefCell::new(None) };
}

/// Tell the pump on this thread which account's link it writes.
pub fn report_on_this_thread(link: Option<VenueLinkStream>) {
    THIS_PUMP.with_borrow_mut(|cell| *cell = link);
}

/// One pump's place in its account's link: a sub-path of the cell. It leaves when the pump ends, unless the pump gave
/// up or PANICKED (dropped while its thread unwinds): that is the reading the operator needs, never the one a clean
/// stop leaves, and the next start's reset of the cell ends it.
pub(super) struct VenueLinkStreamLane {
    /// The pump's sub-path and the account's route key; `None` on a thread nobody told.
    link: Option<(VenueLinkPath, Arc<str>)>,
    keep: bool,
    /// The open session, in `crate::link_history`; none while the stream is down. The cell above is what is true now, and
    /// this is what stays: a drop when the pump ends says we ended it.
    session: Option<VenueLinkRecord>,
}

impl VenueLinkStreamLane {
    /// This thread's pump, not open yet: its sub-path is claimed and reads "opening" (a reopen, `Connecting`).
    pub(super) fn of_this_thread() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let link = THIS_PUMP.with_borrow(Clone::clone).map(|VenueLinkStream(route_key)| {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            (venue_link(&route_key).claim_path(&format!("private stream {id}")), route_key)
        });
        let lane = Self { link, keep: false, session: None };
        lane.write(VenueLinkState::Down(down(
            "the private stream is opening",
            REOPENING_EFFECT,
            0,
            Duration::ZERO,
        )));
        lane
    }

    fn write(&self, state: VenueLinkState) {
        if let Some((path, _)) = &self.link {
            path.set(state, None);
        }
    }

    pub(super) fn open(&mut self) {
        self.write(VenueLinkState::Up);
        if let Some((_, route_key)) = &self.link {
            let thread = std::thread::current();
            self.session = Some(VenueLinkRecord::trade(
                Some(&**route_key),
                thread.name().unwrap_or("user-data"),
            ));
        }
    }

    /// The stream is down and the pump opens it again after `wait`, `failed_opens` failures since it last worked
    /// (`crate::venue_link::VenueLinkDown::failed_restarts`). A failed open ends no session, so it writes no row.
    pub(super) fn down(&mut self, why: &str, failed_opens: u32, wait: Duration) {
        if let Some(session) = self.session.take() {
            session.ended(why);
        }
        self.write(VenueLinkState::Down(down(why, REOPENING_EFFECT, failed_opens, wait)));
    }

    /// The venue refused the key: the pump is ending and nothing opens the stream again.
    pub(super) fn gave_up(&mut self, why: &str, failed_opens: u32) {
        self.keep = true;
        if let Some((path, _)) = &self.link {
            path.gave_up(down(why, GAVE_UP_EFFECT, failed_opens, Duration::ZERO));
        }
    }
}

/// A stream down in the pump's words, its reason cut at [`WHY_LIMIT`].
fn down(why: &str, effect: &'static str, failed_opens: u32, wait: Duration) -> VenueLinkDown {
    VenueLinkDown {
        why: why.chars().take(WHY_LIMIT).collect(),
        effect,
        failed_restarts: failed_opens,
        retry_in: wait,
    }
}

impl Drop for VenueLinkStreamLane {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some(session) = self.session.take() {
                session.ended(PANICKED_WHY);
            }
            if let Some((path, _)) = &self.link {
                path.gave_up(VenueLinkDown::failed(
                    PANICKED_WHY.to_string(),
                    PANICKED_EFFECT,
                    0,
                    Duration::ZERO,
                ));
            }
        } else if !self.keep
            && let Some((path, _)) = &self.link
        {
            path.leave();
        }
    }
}

#[path = "stream_link_tests.rs"]
#[cfg(test)]
mod stream_link_tests;
