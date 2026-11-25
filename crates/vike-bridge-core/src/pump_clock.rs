//! **The clock the shared pumps read**: `crate::websocket_session::user`'s `run_user` and `run_user_resync`,
//! `crate::exec_actor`'s `run_loop` and `crate::websocket_session::market`'s session (its keepalive cadence and
//! reconnect pause) ask THIS module for the time, for a sleep and for a timed channel read, never `Instant::now`,
//! `std::thread::sleep` or `Receiver::recv_timeout` themselves.
//!
//! In every build each answer is the wall clock's. Under `test` or the `test-support` feature a thread that
//! entered a `VirtualClock` (`VirtualClock::enter`) reads that clock instead, so a harness runs a venue's REAL
//! durations (a 60 s idle threshold, a 30 s history lag, a 5 s gap-sentinel window) in milliseconds and
//! deterministically (`crates/vike-bridge-core/tests/link_conformance.rs`). A thread that entered none, which is
//! every thread of a shipped binary, reads the wall clock: the seam changes no pump's behaviour.

#[cfg(any(test, feature = "test-support"))]
use std::cell::RefCell;
#[cfg(any(test, feature = "test-support"))]
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
#[cfg(any(test, feature = "test-support"))]
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Now, on this thread's clock. Public, with [`since`], for a cadence a bridge keeps inside a pump's session (the
/// Client Portal market lane's REST tickle: `crates/bridges/vike-ibkr/src/feed/cpapi/lane.rs`'s `tickle_when_due`), so
/// a harness runs it on the same virtual clock as the pump.
pub fn now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(clock) = entered() {
        return clock.now();
    }
    Instant::now()
}

/// How long since `earlier`, on this thread's clock (`Instant::elapsed`'s twin).
pub fn since(earlier: Instant) -> Duration {
    now().saturating_duration_since(earlier)
}

/// Sleep `span` on this thread's clock.
pub(crate) fn sleep(span: Duration) {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(clock) = entered() {
        clock.sleep(span);
        return;
    }
    std::thread::sleep(span);
}

/// `rx.recv_timeout(wait)` on this thread's clock.
pub(crate) fn recv_timeout<T>(rx: &Receiver<T>, wait: Duration) -> Result<T, RecvTimeoutError> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(clock) = entered() {
        return clock.recv_timeout(rx, wait);
    }
    rx.recv_timeout(wait)
}

/// How finely a timed channel read on a virtual clock looks at its channel: a message another member sent is seen
/// at most this much virtual time late.
#[cfg(any(test, feature = "test-support"))]
const RECV_SLICE: Duration = Duration::from_millis(1);

/// How long (wall clock) a member waits for the others to sleep or leave before the clock calls itself stuck: a
/// member blocked on something that is not this clock stops time for every member.
#[cfg(any(test, feature = "test-support"))]
const STUCK_AFTER: Duration = Duration::from_secs(60);

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static ENTERED: RefCell<Option<Arc<VirtualClock>>> = const { RefCell::new(None) };
}

/// The clock this thread entered, if any.
#[cfg(any(test, feature = "test-support"))]
fn entered() -> Option<Arc<VirtualClock>> {
    ENTERED.with_borrow(Clone::clone)
}

/// **A clock shared by a fixed number of member threads, whose time moves only when every member sleeps.**
///
/// Born at virtual time zero with `members` seats. A thread takes a seat with [`VirtualClock::enter`] and gives it
/// back when the returned [`VirtualClockMember`] drops (its work is done). A sleep never sleeps: the member waits
/// until every member that has not left is asleep, then the clock jumps to the earliest wake time and that member
/// runs. So the order of events across members is the one their virtual times give, whatever the wall clock does,
/// and a 60 s silence costs microseconds.
///
/// ⚠ Every member sleeps on THIS clock or leaves: a member blocked on anything else (a lock, a channel read that is
/// not this module's `recv_timeout`) stops time for all of them, and after `STUCK_AFTER` the clock panics saying so.
#[cfg(any(test, feature = "test-support"))]
pub struct VirtualClock {
    born: Instant,
    state: Mutex<VirtualClockState>,
    turn: Condvar,
}

#[cfg(any(test, feature = "test-support"))]
struct VirtualClockState {
    /// Virtual time since birth.
    now: Duration,
    /// Seats not given back.
    members: usize,
    /// The wake time of each member asleep now.
    asleep: Vec<Duration>,
}

/// A thread's seat on a [`VirtualClock`]: while it lives, this thread's pump clock is that clock.
#[cfg(any(test, feature = "test-support"))]
pub struct VirtualClockMember {
    clock: Arc<VirtualClock>,
}

#[cfg(any(test, feature = "test-support"))]
impl VirtualClock {
    /// A clock at virtual time zero with `members` seats.
    #[must_use]
    pub fn new(members: usize) -> Arc<Self> {
        Arc::new(Self {
            born: Instant::now(),
            state: Mutex::new(VirtualClockState {
                now: Duration::ZERO,
                members,
                asleep: Vec::new(),
            }),
            turn: Condvar::new(),
        })
    }

    /// Take a seat: this thread's pump clock is this clock until the member drops.
    #[must_use]
    pub fn enter(self: &Arc<Self>) -> VirtualClockMember {
        ENTERED.with_borrow_mut(|entered| {
            assert!(entered.is_none(), "this thread already sits on a virtual clock");
            *entered = Some(Arc::clone(self));
        });
        VirtualClockMember { clock: Arc::clone(self) }
    }

    /// Now, on this clock.
    #[must_use]
    pub fn now(&self) -> Instant {
        self.born + self.lock().now
    }

    /// How far `at`, an instant of this clock, is from its birth.
    #[must_use]
    pub fn offset(&self, at: Instant) -> Duration {
        at.saturating_duration_since(self.born)
    }

    fn lock(&self) -> MutexGuard<'_, VirtualClockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A member's sleep of `span` virtual time: wait until every member sleeps, then let the earliest wake. A test's own
    /// member (a thread that sends commands on a schedule) sleeps here; a pump sleeps through this module's `sleep`.
    pub fn sleep(&self, span: Duration) {
        if span.is_zero() {
            return;
        }
        let mut state = self.lock();
        let wake = state.now + span;
        state.asleep.push(wake);
        loop {
            if state.now >= wake {
                let mine = state.asleep.iter().position(|w| *w == wake).expect("my wake time");
                state.asleep.swap_remove(mine);
                return;
            }
            let earliest = state.asleep.iter().copied().min().expect("a sleeper");
            // Every member asleep and none due yet: time jumps to the earliest wake. One due already (it has not run to
            // take itself off) is woken by the jump that made it due: wait for it.
            if state.asleep.len() >= state.members && earliest > state.now {
                state.now = earliest;
                self.turn.notify_all();
                continue;
            }
            let (next, waited) =
                self.turn.wait_timeout(state, STUCK_AFTER).unwrap_or_else(PoisonError::into_inner);
            state = next;
            assert!(
                !waited.timed_out() || state.now >= wake,
                "virtual clock stuck at {:?}: {} of {} member(s) asleep for {STUCK_AFTER:?} of wall clock, so a \
                 member is blocked on something that is not this clock",
                state.now,
                state.asleep.len(),
                state.members
            );
        }
    }

    /// `rx.recv_timeout(wait)` in virtual time: the channel is looked at every `RECV_SLICE`.
    fn recv_timeout<T>(&self, rx: &Receiver<T>, wait: Duration) -> Result<T, RecvTimeoutError> {
        let deadline = self.now() + wait;
        loop {
            match rx.try_recv() {
                Ok(message) => return Ok(message),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }
            let left = deadline.saturating_duration_since(self.now());
            if left.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            self.sleep(left.min(RECV_SLICE));
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for VirtualClockMember {
    fn drop(&mut self) {
        ENTERED.with_borrow_mut(|entered| *entered = None);
        let mut state = self.clock.lock();
        state.members -= 1;
        self.clock.turn.notify_all();
    }
}

#[path = "pump_clock_tests.rs"]
#[cfg(test)]
mod pump_clock_tests;
