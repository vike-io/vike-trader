//! The bounded wait, on a VIRTUAL clock: no sleeping, every boundary exact.

use std::cell::{Cell, RefCell};

use super::*;

/// A clock the wait advances by sleeping, and a log of every sleep it asked for.
struct Virtual {
    t0: Instant,
    elapsed: Cell<Duration>,
    sleeps: RefCell<Vec<Duration>>,
}

impl Virtual {
    fn new() -> Self {
        Virtual {
            t0: Instant::now(),
            elapsed: Cell::new(Duration::ZERO),
            sleeps: RefCell::default(),
        }
    }

    fn elapsed(&self) -> Duration {
        self.elapsed.get()
    }
}

impl Pacer for Virtual {
    fn now(&self) -> Instant {
        self.t0 + self.elapsed.get()
    }

    fn sleep(&self, d: Duration) {
        self.elapsed.set(self.elapsed.get() + d);
        self.sleeps.borrow_mut().push(d);
    }
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused")
}

/// A gatewayhub that is up asks no one to wait: one ask, no sleep.
#[test]
fn an_answer_at_once_costs_no_wait() {
    let clock = Virtual::new();
    let mut asks = 0;
    let got = wait_for(
        &mut || {
            asks += 1;
            Ok::<_, io::Error>("table")
        },
        Duration::from_secs(60),
        &clock,
        "test",
    );
    assert_eq!(got, Ok("table"));
    assert_eq!((asks, clock.sleeps.borrow().len()), (1, 0));
}

/// A gatewayhub that is starting beside the tradehub is found: the pauses double from the first
/// retry to the ceiling and never past it.
#[test]
fn the_pauses_double_to_the_ceiling_and_the_wait_ends_when_it_answers() {
    let clock = Virtual::new();
    let mut failures_left = 6;
    let got = wait_for(
        &mut || {
            if failures_left > 0 {
                failures_left -= 1;
                Err(refused())
            } else {
                Ok(())
            }
        },
        Duration::from_secs(60),
        &clock,
        "test",
    );
    assert_eq!(got, Ok(()));
    let ms: Vec<u128> = clock.sleeps.borrow().iter().map(Duration::as_millis).collect();
    assert_eq!(ms, vec![250, 500, 1000, 2000, 2000, 2000], "doubling, capped at MAX_RETRY");
}

/// **The wait is bounded, and its last ask is made AT the deadline.** One second: asks at 0, 250,
/// 750 and 1000 ms; the last failure is the answer, and not a millisecond more is slept.
#[test]
fn the_wait_ends_at_the_deadline_with_the_last_failure() {
    let clock = Virtual::new();
    let mut asks = 0;
    let got: Result<(), String> = wait_for(
        &mut || {
            asks += 1;
            Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("refused #{asks}")))
        },
        Duration::from_secs(1),
        &clock,
        "test",
    );
    assert_eq!(got, Err("refused #4".to_string()), "the LAST failure is reported");
    assert_eq!(asks, 4);
    assert_eq!(clock.elapsed(), Duration::from_secs(1), "exactly the wait, never more");
}

/// A gatewayhub that answers in the final instant is found: the ask at the deadline counts.
#[test]
fn a_gatewayhub_that_comes_up_at_the_deadline_is_found() {
    let clock = Virtual::new();
    let got = wait_for(
        &mut || {
            if clock.elapsed() >= Duration::from_secs(1) { Ok("up") } else { Err(refused()) }
        },
        Duration::from_secs(1),
        &clock,
        "test",
    );
    assert_eq!(got, Ok("up"));
}

/// A zero wait is one ask: the contract of a start that has nothing to wait for.
#[test]
fn a_zero_wait_asks_exactly_once() {
    let clock = Virtual::new();
    let mut asks = 0;
    let got: Result<(), String> = wait_for(
        &mut || {
            asks += 1;
            Err(refused())
        },
        Duration::ZERO,
        &clock,
        "test",
    );
    assert!(got.is_err());
    assert_eq!((asks, clock.elapsed()), (1, Duration::ZERO));
}
