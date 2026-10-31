//! **The first balance of a reconcile leg** (decisions 0169 and 0246): the cell its pass answers through, and the
//! bounded wait a start can make on it. COLD: a start, never a message.
//!
//! An engine joins the core on `seed_cash`, the wrong figure in both directions, and only the reconcile pass over its
//! leg puts the venue's balance there (`vike_core`'s `reconcile_reports`: the ONE authoritative balance write; this
//! module adds no second). [`FirstBalance`] is what that pass tells the outside about it: whether the balance has landed
//! and, until it has, why not in words. A start that waits on it ([`wait_for_first_balances`]) hands the control
//! door an engine that already trades on the venue's figure; one that cannot wait any longer leaves the cell to the
//! door, which refuses the account's OPENING orders by name and with the reason until the pass answers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::ReconLeg;

/// How long a start waits for the venue balances of its legs before it opens the door anyway
/// (`wait_for_first_balances`'s `bound`). Measured: a hot start's first balance took 5 to 10 s on the okx, bybit and
/// binance demo accounts, and 17 to 21 s when another account's pass was in flight. An account still unanswered at the
/// bound stays refused for opening orders, with its reason, until its pass answers.
// shortcut: a constant, not a `config.*` row (a row is a settings-registry commitment); upgrade to a row when an
// operator has to move it.
pub const FIRST_BALANCE_BOUND: Duration = Duration::from_secs(30);

/// What a leg that has not been visited yet says. Replaced by the reason of the first pass that did not answer, and
/// never read again once the balance has landed.
const NOT_VISITED: &str = "the first reconcile pass has not reached this account yet";

/// The cell a reconcile leg raises when the venue's balance has reached the core's lane, and fills with the reason
/// while it has not.
#[derive(Debug)]
pub struct FirstBalance {
    landed: AtomicBool,
    /// A pass has been over the leg (it landed, or it did not answer and said why).
    visited: AtomicBool,
    /// The last pass over the leg was NOT READY, which the driver repeats within seconds ([`Self::fail_retrying`]).
    retrying: AtomicBool,
    why: Mutex<String>,
}

impl FirstBalance {
    /// A cell nothing has answered: not landed, with the "not reached yet" reason.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(FirstBalance {
            landed: AtomicBool::new(false),
            visited: AtomicBool::new(false),
            retrying: AtomicBool::new(false),
            why: Mutex::new(NOT_VISITED.to_string()),
        })
    }

    /// Whether the leg's first answered balance is on the core's lane. `Acquire` pairs with [`Self::land`]'s `Release`.
    #[must_use]
    pub fn landed(&self) -> bool {
        self.landed.load(Ordering::Acquire)
    }

    /// Why the balance has not landed, in words: the last thing the leg's pass said, or that no pass has reached it.
    #[must_use]
    pub fn why(&self) -> String {
        self.why.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The pass answered: its report is on the core's lane. Called by the manager AFTER the send, so an order the door
    /// lets through on seeing the cell queues behind the report (one FIFO) and is judged on what the pass folded.
    /// Public only so the door's tests (another crate) can play the manager; production has the one caller.
    pub fn land(&self) {
        self.visited.store(true, Ordering::Release);
        self.retrying.store(false, Ordering::Release);
        self.landed.store(true, Ordering::Release);
    }

    /// A pass over the leg did not answer (a suppressed feed, a failed fetch) and none is coming within seconds:
    /// remember why, for the door's words. Public for the same reason as [`Self::land`].
    pub fn fail(&self, why: &str) {
        *self.why.lock().unwrap_or_else(PoisonError::into_inner) = why.to_string();
        self.retrying.store(false, Ordering::Release);
        self.visited.store(true, Ordering::Release);
    }

    /// A pass over the leg was NOT READY (the gatewayhub said it was not attempted: no adapter yet, the link is down,
    /// a boot prefetch is owed) and the driver repeats it within seconds: as [`Self::fail`], but a start keeps waiting
    /// for it, because the next try is likely to answer.
    pub fn fail_retrying(&self, why: &str) {
        *self.why.lock().unwrap_or_else(PoisonError::into_inner) = why.to_string();
        self.retrying.store(true, Ordering::Release);
        self.visited.store(true, Ordering::Release);
    }

    /// A pass has been over the leg, it did not answer, and the next try is the ordinary interval's: waiting for it
    /// longer than the interval changes nothing, and the bound is for a venue that does not answer, not for one that
    /// said no.
    fn settled_without_an_answer(&self) -> bool {
        !self.landed()
            && self.visited.load(Ordering::Acquire)
            && !self.retrying.load(Ordering::Acquire)
    }
}

/// **Give every leg the BOOT builds a first-balance cell** (decision 0246): the boot's engines were left unannounced
/// ("reconciliation seeds them first") although the door opens a couple of seconds before the first pass does. Returns
/// `(route key, cell)` per leg, the route key being the one the control door asks for (`ReconLeg::leg_key`: the
/// account's route key, or the venue for a sole account). A leg that already carries a cell keeps it and is not
/// returned.
#[must_use]
pub fn await_first_balances(legs: &mut [ReconLeg]) -> Vec<(String, Arc<FirstBalance>)> {
    let mut waits = Vec::new();
    for leg in legs {
        if leg.balance_synced.is_none() {
            let cell = FirstBalance::new();
            leg.balance_synced = Some(Arc::clone(&cell));
            waits.push((leg.leg_key().to_string(), cell));
        }
    }
    waits
}

/// **Wait, at most `bound`, until every cell in `waits` has landed**; the route keys still unanswered when the wait ends
/// (all landed: empty). Each account is judged by its own cell, so one that never answers costs the others nothing
/// beyond the one shared bound, and the ones that landed are open. The wait also ends once every cell still down
/// has been visited by a pass that did not answer and will not be tried again within seconds (a failed fetch, a
/// suppressed feed): the bound is for a venue that does not answer, not for one that said no, and a NOT READY leg
/// (which the driver repeats) is waited for. `give_up` ends the wait early (the process stop, a driver that is gone).
/// COLD: polls at 25 ms on the starting thread.
#[must_use]
pub fn wait_for_first_balances(
    waits: &[(String, Arc<FirstBalance>)],
    bound: Duration,
    give_up: impl Fn() -> bool,
) -> Vec<String> {
    let started = Instant::now();
    loop {
        let down: Vec<&(String, Arc<FirstBalance>)> =
            waits.iter().filter(|(_, cell)| !cell.landed()).collect();
        let settled = down.iter().all(|(_, cell)| cell.settled_without_an_answer());
        if down.is_empty() || settled || started.elapsed() >= bound || give_up() {
            return down.into_iter().map(|(route_key, _)| route_key.clone()).collect();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
