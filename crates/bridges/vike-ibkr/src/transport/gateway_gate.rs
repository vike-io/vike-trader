//! **One gate per IB Gateway address**: the connect TURN, the rebuild SCHEDULE of a gateway that lost its
//! link to IBKR, and the port watch that ends that schedule when the operator restarts the gateway
//! (2026-10-17: the gateway lost its link twice, the adapter came up against it and the daemon never healed).
//!
//! One IBKR account admits effectively ONE session (`docs/ops/ibkr-socket-gateway.md` section 5) and the gateway
//! behaves badly under simultaneous or rapid reconnects, while an adapter OPEN dials up to three API clients
//! (exec, the grid pre-fetch, the reconcile client). Three rules, all keyed by the address `host:port`:
//!
//! * **One dial at a time, spaced** ([`GatewayGate::dial`]). Every blocking `ibapi` open step of every account on
//!   the gateway takes the same turn, waits [`CONNECT_GAP`] after the previous one ended, and waits for
//!   the threads still settling (an abandoned open that has not returned yet, a closing pump) so the id of the
//!   previous client is free (error 326 otherwise).
//! * **A rebuild after a LINK LOSS is spaced** ([`Heal`]). Only the link watchdog (`link_watch`) arms it, when
//!   the gateway has had no link to IBKR for 5 minutes: the first rebuild waits 60 s (54 to 66 with the jitter),
//!   then 2 and 5 minutes (the cap), and at most [`MAX_ATTEMPTS_PER_HOUR`] attempts in any hour. A refused attempt dials
//!   NOTHING and costs the supervisor one cheap failed start. A rebuild after any other death (the daily logoff,
//!   a closed port) is not spaced: the gate is not armed. After [`OPERATOR_AFTER_FAILURES`] failures the journal
//!   says, at most every [`OPERATOR_REPEAT`], that the gateway needs an operator: this code NEVER restarts
//!   `vike-ibkr-gateway` (a systemd restart spends the unit's start budget).
//! * **A restarted gateway ends the schedule** ([`PortWatch`]): while armed, a thread watches the port; a port
//!   seen closed twice in a row that opens again is a gateway restart and the outage is over. The rebuild is then
//!   no longer spaced by this gate, but it still waits for the supervisor's own backoff (up to 60 s).
//!
//! * **The data lane never delays the order lane** ([`GatewayGate::dial_data_by`]). A data dial (`IbkrFeeds`,
//!   `HistoricalFetcher`) never QUEUES for the turn: it is refused at once while any dial holds it, while an order-lane
//!   dial waits for it, while the schedule is armed, while its own backoff runs (a data dial the deadline won: 60 s,
//!   2 and 5 min, [`DataDials`]) and when its [`MAX_ATTEMPTS_PER_HOUR`] an hour are spent. One that holds the turn
//!   when an order-lane dial arrives GIVES WAY at once (its open thread is abandoned and counted apart, so the order
//!   lane never waits for it, and the gap is not owed after it). The one caller that RETRIES is the price stream's
//!   supervisor (`crate::redial`): these refusals are its floor, not its pace, and it asks [`GatewayGate::data_ready`]
//!   first, so a refusal that dialled nothing costs it neither an attempt nor a backoff step.
//! * **The hourly data budget is SHARED by two lanes and neither can empty it** ([`DataLane`]): the dials that ASK for
//!   a connection (the stream's first dial, a lookup's, the backfill's: `Lookup`) and the supervisor's re-dial of a
//!   connection it HAD (`Redial`). Each lane may spend at most `MAX_ATTEMPTS_PER_HOUR - LANE_RESERVE` (4) of the six, so
//!   a burst of lookups on a dead gateway cannot starve the re-dial that would heal them (a lookup that finds the
//!   connection down dials it for the stream too, and the stream then JOINS it with no dial), and a stream that
//!   flaps cannot starve a facts lookup.
//!
//! **A connect the deadline abandoned leaks its thread** (and the socket it holds) until `ibapi` lets go: the
//! open is blocking with no timeout of its own. It is counted (a dial waits for the count, up to [`SETTLE_WAIT`])
//! and bounded in number by the schedule: at most [`MAX_ATTEMPTS_PER_HOUR`] attempts an hour once armed.
//!
//! The pure halves ([`Heal`], [`PortWatch`]) take the clock as an argument, so the tests drive a fake one.

use std::collections::{HashMap, VecDeque};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::socket::{bounded, bounded_yielding};
use crate::error::IbkrError;
use crate::redial::backoff;

/// The pause between two dials to one gateway. A test that needs a shorter one builds its own
/// `GatewayGate::new(.., gap, ..)` (a test gate), never this process-wide one.
pub(crate) const CONNECT_GAP: Duration = Duration::from_millis(2_000);

/// The most a dial waits for the threads still settling on its gateway (an abandoned open, a closing pump).
const SETTLE_WAIT: Duration = Duration::from_secs(5);

/// The hard cap on rebuild attempts per hour, whatever the schedule says.
pub(crate) const MAX_ATTEMPTS_PER_HOUR: usize = 6;

/// How many of an hour's [`MAX_ATTEMPTS_PER_HOUR`] data dials each [`DataLane`] leaves to the other.
pub(crate) const LANE_RESERVE: usize = 2;

/// How long a caller waits before it asks [`GatewayGate::data_ready`] again when the answer has no better time (a
/// turn that is held, an armed schedule): the cadence the armed gate looks at the port.
const RECHECK: Duration = PORT_WATCH_EVERY;

/// Failures (the loss itself counts as the first) after which the journal asks for the operator.
pub(crate) const OPERATOR_AFTER_FAILURES: u32 = 3;

/// The operator line is said at most this often.
pub(crate) const OPERATOR_REPEAT: Duration = Duration::from_secs(30 * 60);

/// How often the armed gate looks at the gateway's port.
const PORT_WATCH_EVERY: Duration = Duration::from_secs(3);

const HOUR: Duration = Duration::from_secs(3600);

/// Why a rebuild attempt is refused for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The schedule has not reached the next attempt.
    Spaced { left: Duration },
    /// [`MAX_ATTEMPTS_PER_HOUR`] attempts were made in the last hour.
    Capped { left: Duration },
}

impl Refusal {
    /// How long until the refusal lifts.
    pub(crate) fn left(self) -> Duration {
        match self {
            Refusal::Spaced { left } | Refusal::Capped { left } => left,
        }
    }
}

/// The rebuild schedule of a gateway that lost its link. Unarmed until [`Heal::lost`].
#[derive(Debug, Default)]
pub(crate) struct Heal {
    /// When the outage that armed it began (`None` = unarmed).
    down_since: Option<Instant>,
    /// Losses and failed rebuilds since the arm: the index into the backoff table (`crate::redial::backoff`).
    failures: u32,
    /// The earliest next attempt.
    next_ok: Option<Instant>,
    /// The attempts admitted in the last hour.
    attempts: VecDeque<Instant>,
    /// When the operator line was last said.
    said_at: Option<Instant>,
}

impl Heal {
    pub(crate) fn is_armed(&self) -> bool {
        self.down_since.is_some()
    }

    /// How long the armed outage has lasted (zero when unarmed).
    pub(crate) fn down_for(&self, now: Instant) -> Duration {
        self.down_since.map_or(Duration::ZERO, |d| now.saturating_duration_since(d))
    }

    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    /// The wait after `failures` losses and failed rebuilds, `jitter_permille` (-100..=100) off the table. The
    /// table is the data lane's backoff too ([`DataDials::timed_out`]) and the price stream's re-dial schedule
    /// (`crate::redial`).
    pub(crate) fn wait_for(failures: u32, jitter_permille: i32) -> Duration {
        let base = backoff(failures);
        base.mul_f64((1000 + i64::from(jitter_permille.clamp(-100, 100))) as f64 / 1000.0)
    }

    /// The watchdog declared the link lost at `now`, the outage having begun at `began`. A second loss inside the
    /// wait the first one set is the same outage (two accounts on one gateway) and counts once.
    pub(crate) fn lost(&mut self, began: Instant, now: Instant, jitter_permille: i32) {
        if self.is_armed() && self.next_ok.is_some_and(|n| now < n) {
            return;
        }
        self.down_since.get_or_insert(began);
        self.failures += 1;
        self.next_ok = Some(now + Self::wait_for(self.failures, jitter_permille));
    }

    /// An admitted exec connect failed. An armed schedule backs off; an unarmed one arms only on a TIMEOUT (the
    /// gateway took the connection and never answered: its link to IBKR is down), not on a refusal (the port
    /// is closed: a restart, which is not spaced).
    pub(crate) fn attempt_failed(&mut self, now: Instant, timed_out: bool, jitter_permille: i32) {
        if self.is_armed() {
            self.failures += 1;
            self.next_ok = Some(now + Self::wait_for(self.failures, jitter_permille));
        } else if timed_out {
            self.lost(now, now, jitter_permille);
        }
    }

    /// Whether an exec connect may be dialled now; an admitted attempt is counted.
    pub(crate) fn permit(&mut self, now: Instant) -> Result<(), Refusal> {
        if !self.is_armed() {
            return Ok(());
        }
        if let Some(next) = self.next_ok.filter(|n| now < *n) {
            return Err(Refusal::Spaced { left: next - now });
        }
        while self.attempts.front().is_some_and(|a| now.saturating_duration_since(*a) >= HOUR) {
            self.attempts.pop_front();
        }
        if self.attempts.len() >= MAX_ATTEMPTS_PER_HOUR {
            let oldest = self.attempts.front().copied().unwrap_or(now);
            return Err(Refusal::Capped { left: (oldest + HOUR).saturating_duration_since(now) });
        }
        self.attempts.push_back(now);
        Ok(())
    }

    /// The link is healthy again (a session ran clean through its probation, or the gateway restarted).
    pub(crate) fn healthy(&mut self) {
        *self = Self::default();
    }

    /// The operator line, when it is due: armed, [`OPERATOR_AFTER_FAILURES`] failures, and not said within
    /// [`OPERATOR_REPEAT`]. Marks it said.
    pub(crate) fn operator_note(&mut self, now: Instant) -> Option<String> {
        if !self.is_armed() || self.failures < OPERATOR_AFTER_FAILURES {
            return None;
        }
        if self.said_at.is_some_and(|s| now.saturating_duration_since(s) < OPERATOR_REPEAT) {
            return None;
        }
        self.said_at = Some(now);
        Some(format!(
            "IB Gateway has no link to IBKR for {} min: restart vike-ibkr-gateway (operator). The session was lost or failed \
             to rebuild {} times; the daemon keeps retrying on its spaced schedule and never restarts the gateway itself",
            self.down_for(now).as_secs() / 60,
            self.failures
        ))
    }
}

/// The DATA lane's own budget and backoff on one gateway (the order lane's is [`Heal`]): at most
/// [`MAX_ATTEMPTS_PER_HOUR`] data dials an hour, and a dial the deadline won (the gateway took the socket and never
/// answered) spaces the next one on the same 60 s, 2 min, 5 min schedule as [`Heal`], so a feed that retries on a
/// silent gateway cannot keep dialling it. A refusal, a yield to the order lane and a dial that was answered (even
/// with an error) back off nothing. Pure: every method takes the clock.
#[derive(Debug, Default)]
pub(crate) struct DataDials {
    /// Data dials the deadline won since the last answered one: the index into the backoff table.
    timeouts: u32,
    /// The earliest next data dial.
    next_ok: Option<Instant>,
    /// The data dials admitted in the last hour, with the lane each was for.
    attempts: VecDeque<(Instant, DataLane)>,
}

/// Which caller a data dial is for; see the module doc for the budget they share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataLane {
    /// A caller that ASKS for a connection: the price stream's first dial, a lookup's, the backfill's.
    Lookup,
    /// The price stream's supervised re-dial of a connection it HAD (`crate::redial`).
    Redial,
}

impl DataDials {
    /// Whether a data dial for `lane` may start at `now`. Counts nothing ([`Self::admit`] does). The hour's six are
    /// everyone's; one `lane` alone may hold at most `MAX_ATTEMPTS_PER_HOUR - LANE_RESERVE` of them.
    pub(crate) fn check(&mut self, now: Instant, lane: DataLane) -> Result<(), Refusal> {
        if let Some(next) = self.next_ok.filter(|n| now < *n) {
            return Err(Refusal::Spaced { left: next - now });
        }
        while self.attempts.front().is_some_and(|(a, _)| now.saturating_duration_since(*a) >= HOUR)
        {
            self.attempts.pop_front();
        }
        // Attempts are pushed in time order, so the first match is the oldest.
        let frees_in = |of: Option<DataLane>| {
            let oldest = self.attempts.iter().find(|(_, l)| of.is_none_or(|x| *l == x));
            let oldest = oldest.map_or(now, |(a, _)| *a);
            Refusal::Capped { left: (oldest + HOUR).saturating_duration_since(now) }
        };
        if self.attempts.len() >= MAX_ATTEMPTS_PER_HOUR {
            return Err(frees_in(None));
        }
        let own = self.attempts.iter().filter(|(_, l)| *l == lane).count();
        if own >= MAX_ATTEMPTS_PER_HOUR - LANE_RESERVE {
            return Err(frees_in(Some(lane)));
        }
        Ok(())
    }

    /// A data dial for `lane` starts at `now`: it is spent from the hour's budget.
    pub(crate) fn admit(&mut self, now: Instant, lane: DataLane) {
        self.attempts.push_back((now, lane));
    }

    /// The deadline won a data dial that ended at `now`: back the next one off.
    pub(crate) fn timed_out(&mut self, now: Instant, jitter_permille: i32) {
        self.timeouts += 1;
        self.next_ok = Some(now + Heal::wait_for(self.timeouts, jitter_permille));
    }

    /// The gateway answered a data dial: the backoff is over (the hour's budget stays spent).
    pub(crate) fn answered(&mut self) {
        self.timeouts = 0;
        self.next_ok = None;
    }
}

/// Tells a gateway RESTART (the port closed, then opened) from a port that never moved or blinked once.
#[derive(Debug, Default)]
pub(crate) struct PortWatch {
    /// Consecutive samples that found the port closed.
    closed_run: u32,
    /// The port was closed in two samples in a row.
    closed_seen: bool,
}

impl PortWatch {
    /// One look at the port; `true` once, when it is open again after having been seen closed TWICE in a row (a
    /// single refused sample is a busy gateway, not a restart).
    pub(crate) fn observe(&mut self, open: bool) -> bool {
        if !open {
            self.closed_run += 1;
            self.closed_seen |= self.closed_run >= 2;
            return false;
        }
        self.closed_run = 0;
        std::mem::take(&mut self.closed_seen)
    }
}

/// Counts the threads still settling on a gateway: an open step abandoned by its deadline and not yet
/// returned, a pump closing its client. A dial waits for zero. Held by a [`Settling`] guard.
pub(crate) type SettleCount = Arc<AtomicUsize>;

/// One thing settling; counted from construction to drop.
pub(crate) struct Settling(SettleCount);

impl Settling {
    pub(crate) fn hold(count: &SettleCount) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(count))
    }
}

impl Drop for Settling {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// **One ask's OVERALL deadline** (a facts ask: the account's base currency, a stock's facts, a forex rate): the
/// bound that covers everything the ask waits for, not only the open step. A data dial's own `within` starts when
/// the open step starts, after the wait for the session's lock, the settling threads ([`SETTLE_WAIT`], 5 s) and the
/// [`CONNECT_GAP`] (2 s), so an ask bounded only by it could take its bound plus all of those, with the account's
/// adapter lock held, and an admitted dial could leave the read after it no time at all. Under a deadline the data
/// dial waits for none of those past it, hands its open step what is LEFT, and is not admitted (so it spends none of
/// the lane's six hourly dials) when less than [`Self::worth_a_dial`] is left.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AskDeadline {
    by: Instant,
    worth_a_dial: Duration,
}

impl AskDeadline {
    /// A deadline `total` from now. A dial is worth its hourly place only while a quarter of `total` is left: the
    /// connect and the first request after it fit in less than that on a gateway that answers at all, and a
    /// dial that leaves less would be spent for a read that cannot finish.
    pub(crate) fn within(total: Duration) -> Self {
        Self { by: Instant::now() + total, worth_a_dial: total / 4 }
    }

    /// The instant the ask's time is up.
    pub(crate) fn by(&self) -> Instant {
        self.by
    }

    /// What is left of the ask's time (zero once it is up).
    pub(crate) fn left(&self) -> Duration {
        self.by.saturating_duration_since(Instant::now())
    }

    /// Whether the ask's time is up.
    fn is_up(&self) -> bool {
        self.left().is_zero()
    }
}

/// -100..=100 permille of jitter from the clock's nanoseconds (no random-number crate for one spread).
fn jitter_permille() -> i32 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    (nanos % 201) as i32 - 100
}

/// The gate of one gateway address. See the module doc.
pub(crate) struct GatewayGate {
    addr: String,
    /// When the last dial ended; holding the lock IS having the turn.
    last_dial: Mutex<Option<Instant>>,
    settling: SettleCount,
    /// Order-lane dials ([`Self::dial`]) waiting for or holding the turn: a data dial never queues behind them and
    /// gives way to one that arrives while it waits.
    order_pending: SettleCount,
    /// Threads an abandoned DATA open still holds. Only data dials wait for them (the order lane never does).
    data_settling: SettleCount,
    /// The data lane's hourly budget and backoff.
    data: Mutex<DataDials>,
    heal: Mutex<Heal>,
    gap: Duration,
    /// Whether an armed gate watches the port (off for a test gate).
    port_watch: bool,
    watching: AtomicBool,
}

static GATES: LazyLock<Mutex<HashMap<String, Arc<GatewayGate>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The process-wide gate of `addr` (`host:port`).
pub(crate) fn gate_for(addr: &str) -> Arc<GatewayGate> {
    Arc::clone(
        lock(&GATES)
            .entry(addr.to_string())
            .or_insert_with(|| Arc::new(GatewayGate::new(addr, CONNECT_GAP, true))),
    )
}

impl GatewayGate {
    pub(crate) fn new(addr: &str, gap: Duration, port_watch: bool) -> Self {
        Self {
            addr: addr.to_string(),
            last_dial: Mutex::new(None),
            settling: Arc::new(AtomicUsize::new(0)),
            order_pending: Arc::new(AtomicUsize::new(0)),
            data_settling: Arc::new(AtomicUsize::new(0)),
            data: Mutex::new(DataDials::default()),
            heal: Mutex::new(Heal::default()),
            gap,
            port_watch,
            watching: AtomicBool::new(false),
        }
    }

    /// The count a closing pump holds ([`Settling::hold`]).
    pub(crate) fn settling(&self) -> &SettleCount {
        &self.settling
    }

    /// Whether the schedule is armed (a test reads it).
    #[cfg(test)]
    pub(crate) fn is_armed(&self) -> bool {
        lock(&self.heal).is_armed()
    }

    /// The count of settling threads (a test reads it).
    #[cfg(test)]
    pub(crate) fn settling_now(&self) -> usize {
        self.settling.load(Ordering::SeqCst)
    }

    /// The data dials admitted in the last hour, i.e. what the callers of this gate have spent of the lane's
    /// hourly budget (a test counts the dials an ask cost).
    #[cfg(test)]
    pub(crate) fn data_dials_spent(&self) -> usize {
        lock(&self.data).attempts.len()
    }

    /// Run ONE blocking `ibapi` open step under the turn: after the previous dial and the settling threads, spaced
    /// by the gap, bounded by `within` (`crate::transport::socket::bounded`). The flag says the DEADLINE won (the
    /// gateway took the connection and never answered).
    pub(crate) fn dial<T: Send + 'static>(
        &self,
        what: &str,
        within: Duration,
        f: impl FnOnce() -> Result<T, IbkrError> + Send + 'static,
    ) -> (Result<T, IbkrError>, bool) {
        // Announced BEFORE the wait for the turn, so a data dial holding it sees this one and gives way.
        let _pending = Settling::hold(&self.order_pending);
        let mut last = lock(&self.last_dial);
        let deadline = Instant::now() + SETTLE_WAIT;
        while self.settling.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                tracing::warn!(
                    gateway = %self.addr,
                    "ibkr: dialling `{what}` although an earlier open or close on this gateway has not returned \
                     after {}s: its client id may still be held (error 326)",
                    SETTLE_WAIT.as_secs()
                );
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if let Some(end) = *last {
            let ready = end + self.gap;
            let now = Instant::now();
            if ready > now {
                thread::sleep(ready - now);
            }
        }
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        let out = bounded(what, within, Some(&self.settling), move || {
            let r = f();
            done.store(true, Ordering::SeqCst);
            r
        });
        *last = Some(Instant::now());
        let timed_out = out.is_err() && !finished.load(Ordering::SeqCst);
        (out, timed_out)
    }

    /// Whether an EXEC connect may be dialled now. An armed schedule refuses and nothing is dialled. ⚠ The words
    /// of the refusal reach the JOURNAL only (the start's `warn!` carries the error): the host folds a failed start
    /// into a generic `AdapterFailed` reason, which does not repeat them.
    pub(crate) fn permit_exec(&self) -> Result<(), IbkrError> {
        let now = Instant::now();
        let (verdict, note, down) = {
            let mut heal = lock(&self.heal);
            let verdict = heal.permit(now);
            (verdict, heal.operator_note(now), heal.down_for(now))
        };
        if let Some(note) = note {
            tracing::error!(gateway = %self.addr, "ibkr: {note}");
        }
        verdict.map_err(|why| {
            let (what, left) = match why {
                Refusal::Spaced { left } => ("the next rebuild attempt is allowed in", left),
                Refusal::Capped { left } => {
                    ("the hourly cap of rebuild attempts is spent; the next is allowed in", left)
                }
            };
            IbkrError::Connect(format!(
                "IB Gateway link watchdog: the gateway at {} has had no link to IBKR for {} s, so its session is rebuilt \
                 on a spaced schedule (about 60 s after the loss, then 2 and 5 min, at most {MAX_ATTEMPTS_PER_HOUR} \
                 per hour); {what} {} s and nothing was dialled",
                self.addr,
                down.as_secs(),
                left.as_secs()
            ))
        })
    }

    /// Whether a DATA-lane connect (`IbkrFeeds`, `HistoricalFetcher`) may be dialled now: never while the schedule is
    /// armed, so the data lane cannot take the exec rebuild's turn nor spend its hourly attempts (nothing is counted).
    /// The same watchdog words as [`Self::permit_exec`]; nothing is dialled.
    pub(crate) fn permit_data(&self) -> Result<(), IbkrError> {
        let (armed, down) = {
            let heal = lock(&self.heal);
            (heal.is_armed(), heal.down_for(Instant::now()))
        };
        if !armed {
            return Ok(());
        }
        Err(IbkrError::Connect(format!(
            "IB Gateway link watchdog: the gateway at {} has had no link to IBKR for {} s, so its session is rebuilt on a \
             spaced schedule and the data connection waits for it; nothing was dialled",
            self.addr,
            down.as_secs()
        )))
    }

    /// The gate's own tests' spelling of [`Self::dial_data_by`] with no deadline; production calls that one.
    #[cfg(test)]
    pub(crate) fn dial_data<T: Send + 'static>(
        &self,
        lane: DataLane,
        what: &str,
        within: Duration,
        f: impl FnOnce() -> Result<T, IbkrError> + Send + 'static,
    ) -> Result<T, IbkrError> {
        self.dial_data_by(lane, what, within, None, f)
    }

    /// Run ONE blocking `ibapi` open step of the DATA lane (`IbkrFeeds`, `HistoricalFetcher`) so that it can never
    /// delay an order-lane dial ([`Self::dial`]: the exec connect, the grid pre-fetch, the reconcile connect):
    ///
    /// * refused (nothing dialled) while the rebuild schedule is armed ([`Self::permit_data`]), while the data
    ///   lane's own backoff runs or its hourly budget is spent ([`DataDials`]), while ANY dial holds the turn (a data
    ///   dial never queues for it) and while an order-lane dial is waiting for it;
    /// * it GIVES WAY the moment an order-lane dial arrives, whether it is still waiting out the settling threads and
    ///   the [`CONNECT_GAP`] or is in the open itself: the open thread is abandoned (counted in `data_settling`,
    ///   which only data dials wait for), and the gap is not charged to the order lane for it;
    /// * a dial the deadline won arms the data backoff; an answered one clears it.
    ///
    /// `lane` is whose share of the hourly budget the dial spends ([`DataLane`]).
    ///
    /// `ask` is the OVERALL deadline of a facts ask ([`AskDeadline`]), `None` for every other caller (the price
    /// stream, the backfill): with one, the waits for the settling threads and the gap end at it (refused, nothing
    /// admitted), the open step gets `within` or what is left of the ask, whichever is less, and a dial that would
    /// leave the ask less than its worth is not admitted. Without one the open step is bounded by `within` alone.
    ///
    /// Lock order: [`Self::permit_data`] takes and releases `heal` first, then the turn (`last_dial`) is TRIED, then
    /// `data` is taken for a moment; nothing holds two of them while waiting.
    pub(crate) fn dial_data_by<T: Send + 'static>(
        &self,
        lane: DataLane,
        what: &str,
        within: Duration,
        ask: Option<AskDeadline>,
        f: impl FnOnce() -> Result<T, IbkrError> + Send + 'static,
    ) -> Result<T, IbkrError> {
        self.permit_data()?;
        lock(&self.data).check(Instant::now(), lane).map_err(|why| self.data_refusal(why))?;
        let waiting = || self.order_pending.load(Ordering::SeqCst) > 0;
        let refused = |why: &str| {
            IbkrError::Connect(format!(
                "IB Gateway data lane: {why} (the data connection `{what}` of {} is retried later); nothing was dialled",
                self.addr
            ))
        };
        let out_of_time = || ask.is_some_and(|ask| ask.is_up());
        let ran_out = || refused("the ask's time ran out before the connect could start");
        let mut last = match self.last_dial.try_lock() {
            Ok(turn) => turn,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(refused("another connect holds the gateway's turn"));
            }
        };
        let deadline = Instant::now() + SETTLE_WAIT;
        while self.settling.load(Ordering::SeqCst) > 0
            || self.data_settling.load(Ordering::SeqCst) > 0
        {
            if waiting() {
                return Err(refused("an order-lane connect is waiting for the turn"));
            }
            if out_of_time() {
                return Err(ran_out());
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    gateway = %self.addr,
                    "ibkr: dialling `{what}` although an earlier open or close on this gateway has not returned \
                     after {}s: its client id may still be held (error 326)",
                    SETTLE_WAIT.as_secs()
                );
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if let Some(end) = *last {
            let ready = end + self.gap;
            loop {
                let now = Instant::now();
                if ready <= now {
                    break;
                }
                if waiting() {
                    return Err(refused("an order-lane connect is waiting for the turn"));
                }
                if out_of_time() {
                    return Err(ran_out());
                }
                thread::sleep((ready - now).min(Duration::from_millis(25)));
            }
        }
        if waiting() {
            return Err(refused("an order-lane connect is waiting for the turn"));
        }
        // The open step gets what is left of the ask, and a dial that cannot be worth its hourly place is not
        // admitted: the waits above are over, so this is the time the connect and the read after it really have.
        let within = match ask {
            None => within,
            Some(ask) if ask.left() < ask.worth_a_dial || ask.is_up() => {
                return Err(refused(&format!(
                    "{} ms of the ask's time are left after the waits for the turn, less than a connect is worth",
                    ask.left().as_millis()
                )));
            }
            Some(ask) => within.min(ask.left()),
        };
        lock(&self.data).admit(Instant::now(), lane);
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        let mut gave_way = false;
        let out = bounded_yielding(
            what,
            within,
            Some(&self.data_settling),
            || {
                gave_way = waiting();
                gave_way
            },
            move || {
                let r = f();
                done.store(true, Ordering::SeqCst);
                r
            },
        );
        // A dial that gave way ended nothing the gateway answered: the gap after it is not owed, so the order lane
        // that took the turn is not held back by it.
        if !gave_way {
            *last = Some(Instant::now());
        }
        let timed_out = out.is_err() && !gave_way && !finished.load(Ordering::SeqCst);
        if timed_out {
            lock(&self.data).timed_out(Instant::now(), jitter_permille());
            tracing::warn!(
                gateway = %self.addr,
                "ibkr: the data connection `{what}` took the gateway's socket and never answered; data connects are \
                 spaced (about 60 s, then 2 and 5 min) and the order lane is not delayed by them"
            );
        } else if !gave_way {
            lock(&self.data).answered();
        }
        out
    }

    /// Whether a data dial for `lane` could start RIGHT NOW without being refused, else how long to wait before
    /// asking again. Spends nothing and reserves nothing (the dial checks again): a caller that retries asks first, so
    /// a refusal that dialled nothing costs it no attempt and no backoff step. The turn counts as taken while any
    /// dial holds it or an order-lane dial waits for it, and the armed rebuild schedule refuses outright.
    pub(crate) fn data_ready(&self, lane: DataLane) -> Result<(), Duration> {
        if lock(&self.heal).is_armed() || self.order_pending.load(Ordering::SeqCst) > 0 {
            return Err(RECHECK);
        }
        lock(&self.data).check(Instant::now(), lane).map_err(Refusal::left)?;
        match self.last_dial.try_lock() {
            Err(TryLockError::WouldBlock) => Err(RECHECK),
            _ => Ok(()),
        }
    }

    /// The words of a data dial refused by its own backoff or budget.
    fn data_refusal(&self, why: Refusal) -> IbkrError {
        let (what, left) = match why {
            Refusal::Spaced { left } => ("the next data connect is allowed in", left),
            Refusal::Capped { left } => (
                "the hourly cap of data connects (this lane's share of it, or all six) is spent; the next is allowed in",
                left,
            ),
        };
        IbkrError::Connect(format!(
            "IB Gateway data lane: data connects to {} are spaced (a data connect that took the socket and never \
             answered backs the next off about 60 s, then 2 and 5 min; at most {MAX_ATTEMPTS_PER_HOUR} an hour); \
             {what} {} s and nothing was dialled",
            self.addr,
            left.as_secs()
        ))
    }

    /// An admitted exec connect ended in failure; `timed_out` = the deadline won.
    pub(crate) fn exec_failed(self: &Arc<Self>, timed_out: bool) {
        let armed = {
            let mut heal = lock(&self.heal);
            let was = heal.is_armed();
            heal.attempt_failed(Instant::now(), timed_out, jitter_permille());
            !was && heal.is_armed()
        };
        if armed {
            tracing::warn!(
                gateway = %self.addr,
                "ibkr: the gateway took the connection and never answered: it has no link to IBKR; the next rebuilds \
                 are spaced (about 60 s, then 2 and 5 min)"
            );
            self.watch_port();
        }
    }

    /// The link watchdog declared the link lost; the outage began at `began`.
    pub(crate) fn lost(self: &Arc<Self>, began: Instant) {
        let now = Instant::now();
        let (failures, left) = {
            let mut heal = lock(&self.heal);
            heal.lost(began, now, jitter_permille());
            (
                heal.failures(),
                heal.next_ok.map_or(Duration::ZERO, |n| n.saturating_duration_since(now)),
            )
        };
        tracing::warn!(
            gateway = %self.addr,
            failures,
            "ibkr: link watchdog: the gateway has no link to IBKR; the next rebuild of the session is allowed in {} s \
             (rebuilds are spaced: about 60 s after the loss, then 2 and 5 min)",
            left.as_secs()
        );
        self.watch_port();
    }

    /// A session ran clean through its probation: the outage is over.
    pub(crate) fn healthy(&self) {
        let mut heal = lock(&self.heal);
        if heal.is_armed() {
            tracing::info!(
                gateway = %self.addr,
                "ibkr: link watchdog: the gateway's link is healthy again; rebuilds are no longer spaced"
            );
        }
        heal.healthy();
    }

    /// Watch the gateway's port while armed: closed, then open again = the gateway restarted = the outage is over.
    fn watch_port(self: &Arc<Self>) {
        if !self.port_watch || self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        let gate = Arc::clone(self);
        let spawned = thread::Builder::new().name("ibkr-port-watch".into()).spawn(move || {
            loop {
                let mut watch = PortWatch::default();
                while lock(&gate.heal).is_armed() {
                    thread::sleep(PORT_WATCH_EVERY);
                    if watch.observe(port_answers(&gate.addr)) {
                        tracing::info!(
                            gateway = %gate.addr,
                            "ibkr: link watchdog: the gateway's port closed and opened again (a restart): the outage \
                             is over and the next rebuild is no longer spaced by the watchdog"
                        );
                        lock(&gate.heal).healthy();
                    }
                }
                // Give the slot back, then look once more: a loss that armed the gate while this thread was
                // finishing found the slot taken and started no watch, so it is taken again here.
                gate.watching.store(false, Ordering::SeqCst);
                if !lock(&gate.heal).is_armed() || gate.watching.swap(true, Ordering::SeqCst) {
                    break;
                }
            }
        });
        if spawned.is_err() {
            self.watching.store(false, Ordering::SeqCst);
        }
    }

    /// The schedule itself, for a test.
    #[cfg(test)]
    pub(crate) fn heal(&self) -> MutexGuard<'_, Heal> {
        lock(&self.heal)
    }
}

/// Whether one TCP connect to `addr` is answered within half a second. Nothing is sent.
pub(crate) fn port_answers(addr: &str) -> bool {
    addr.to_socket_addrs().is_ok_and(|mut it| {
        it.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok())
    })
}

#[cfg(test)]
#[path = "gateway_gate_tests.rs"]
mod gateway_gate_tests;
