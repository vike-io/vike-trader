//! The reconnect loop against a scripted link and a pause that records instead of sleeping.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

use super::{Backoff, Link, Pause, Schedule, Verdict, supervise};

const SEC: Duration = Duration::from_secs(1);

fn schedule() -> Schedule {
    Schedule { probe_every: SEC, first_wait: 2 * SEC, max_wait: 8 * SEC }
}

/// A link that plays back scripts: `reachable` answers pop from a queue (the last one repeats), as do
/// `verdict` and `restore`. Every call is recorded in order.
struct Script {
    reachable: RefCell<VecDeque<bool>>,
    verdicts: RefCell<VecDeque<Verdict>>,
    restores: RefCell<VecDeque<Result<(), String>>>,
    reported: RefCell<VecDeque<Option<String>>>,
    log: RefCell<Vec<String>>,
}

impl Script {
    fn new(reachable: &[bool], verdicts: &[Verdict], restores: &[Result<(), String>]) -> Self {
        Self {
            reachable: RefCell::new(reachable.iter().copied().collect()),
            verdicts: RefCell::new(verdicts.iter().cloned().collect()),
            restores: RefCell::new(restores.iter().cloned().collect()),
            reported: RefCell::new(VecDeque::new()),
            log: RefCell::new(Vec::new()),
        }
    }

    /// What the thing behind the link says of itself, one answer per UP pass (then nothing).
    fn reporting(self, said: &[Option<&str>]) -> Self {
        *self.reported.borrow_mut() = said.iter().map(|s| s.map(str::to_string)).collect();
        self
    }

    fn pop<T: Clone>(q: &RefCell<VecDeque<T>>) -> T {
        let mut q = q.borrow_mut();
        if q.len() > 1 { q.pop_front().expect("non-empty") } else { q[0].clone() }
    }

    fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

impl Link for Script {
    fn verdict(&self) -> Verdict {
        let v = Self::pop(&self.verdicts);
        let word = match &v {
            Verdict::Go => "true",
            Verdict::Stop(_) => "false",
            Verdict::Retry(_) => "retry",
        };
        self.log.borrow_mut().push(format!("verdict:{word}"));
        v
    }
    fn reachable(&self) -> Result<(), String> {
        let up = Self::pop(&self.reachable);
        self.log.borrow_mut().push(format!("reachable:{up}"));
        if up { Ok(()) } else { Err("refused".into()) }
    }
    fn reported_lost(&self) -> Option<String> {
        let said = self.reported.borrow_mut().pop_front().flatten();
        if let Some(why) = &said {
            self.log.borrow_mut().push(format!("reported:{why}"));
        }
        said
    }
    fn lost(&self, why: &str) {
        self.log.borrow_mut().push(format!("lost:{why}"));
    }
    fn restore(&self) -> Result<(), String> {
        let r = Self::pop(&self.restores);
        self.log.borrow_mut().push(format!("restore:{}", r.is_ok()));
        r
    }
    fn gave_up(&self, why: &str) {
        self.log.borrow_mut().push(format!("gave_up:{why}"));
    }
}

/// Grants `budget` pauses, recording each; the next one is refused (stop).
struct Budgeted {
    budget: Cell<usize>,
    waits: RefCell<Vec<Duration>>,
}

impl Budgeted {
    fn new(budget: usize) -> Self {
        Self { budget: Cell::new(budget), waits: RefCell::new(Vec::new()) }
    }
}

impl Pause for Budgeted {
    fn pause(&self, for_: Duration) -> bool {
        self.waits.borrow_mut().push(for_);
        if self.budget.get() == 0 {
            return false;
        }
        self.budget.set(self.budget.get() - 1);
        true
    }
}

#[test]
fn a_loss_is_noticed_once_and_the_first_attempt_is_not_delayed() {
    // up, up, DOWN twice (lost), then the port answers at once and the restore succeeds, then it stays up.
    let link = Script::new(&[true, true, false, false, true, true], &[Verdict::Go], &[Ok(())]);
    let pause = Budgeted::new(6);
    supervise(&link, &schedule(), &pause);
    let log = link.log();
    let lost = log.iter().position(|l| l.starts_with("lost:")).expect("a loss was reported");
    assert_eq!(log.iter().filter(|l| l.starts_with("lost:")).count(), 1, "{log:?}");
    assert_eq!(
        &log[lost..lost + 4],
        ["lost:refused", "verdict:true", "reachable:true", "restore:true"],
        "the rows were asked, then the port, then the restore, with no wait before the first attempt: {log:?}"
    );
}

#[test]
fn the_rows_are_read_before_every_attempt_and_a_stop_ends_the_loop() {
    // Lost; the port answers; the first restore fails; the rows still say go; then they say stop.
    let link = Script::new(
        &[false, false, true],
        &[Verdict::Go, Verdict::Go, Verdict::Stop("account inactive".into())],
        &[Err("login pending".into())],
    );
    let pause = Budgeted::new(20);
    supervise(&link, &schedule(), &pause);
    let log = link.log();
    assert_eq!(log.last().map(String::as_str), Some("gave_up:account inactive"), "{log:?}");
    let restores = log.iter().filter(|l| l.starts_with("restore:")).count();
    assert_eq!(restores, 1, "no attempt was made after the rows said stop: {log:?}");
    let verdicts = log.iter().filter(|l| l.starts_with("verdict:")).count();
    assert_eq!(verdicts, 3, "the rows were read on every pass: {log:?}");
}

#[test]
fn a_stop_in_the_rows_ends_the_loop_even_while_the_port_is_closed() {
    // The gateway never comes back; the rows are what ends it.
    let link = Script::new(
        &[false],
        &[Verdict::Go, Verdict::Go, Verdict::Stop("deactivated".into())],
        &[Ok(())],
    );
    let pause = Budgeted::new(50);
    supervise(&link, &schedule(), &pause);
    let log = link.log();
    assert!(log.iter().all(|l| !l.starts_with("restore:")), "nothing was restored: {log:?}");
    assert_eq!(log.last().map(String::as_str), Some("gave_up:deactivated"));
}

#[test]
fn failed_restores_wait_twice_as_long_each_time_up_to_the_ceiling() {
    let link = Script::new(
        &[false, false, true],
        &[Verdict::Go],
        &[Err("a".into()), Err("b".into()), Err("c".into()), Err("d".into()), Ok(())],
    );
    // Count the passes between consecutive restore attempts.
    let pause = Budgeted::new(60);
    supervise(&link, &schedule(), &pause);
    let log = link.log();
    let mut gaps = Vec::new();
    let mut since = 0usize;
    let mut seen_first = false;
    for line in &log {
        if line.starts_with("restore:") {
            if seen_first {
                gaps.push(since);
            }
            seen_first = true;
            since = 0;
        } else if line.starts_with("verdict:") {
            since += 1;
        }
    }
    // Waits of 2 s, 4 s, 8 s (the ceiling), 8 s at a 1 s pass: that many passes read the rows before the next attempt.
    assert_eq!(gaps, [2, 4, 8, 8], "{log:?}");
}

#[test]
fn a_pause_that_says_stop_ends_the_loop_in_either_state() {
    let up = Script::new(&[true], &[Verdict::Go], &[Ok(())]);
    supervise(&up, &schedule(), &Budgeted::new(0));
    assert_eq!(up.log(), Vec::<String>::new(), "stopped before the first probe");

    let down = Script::new(&[false], &[Verdict::Go], &[Ok(())]);
    supervise(&down, &schedule(), &Budgeted::new(2));
    let log = down.log();
    assert!(log.iter().any(|l| l.starts_with("lost:")));
    assert!(log.iter().all(|l| !l.starts_with("restore:")));
}

#[test]
fn a_restored_link_is_watched_again_and_the_backoff_starts_over() {
    // Lost, restored, lost again, restored again: two losses, two restores.
    let link = Script::new(
        &[true, false, false, true, true, false, false, true, true],
        &[Verdict::Go],
        &[Ok(())],
    );
    supervise(&link, &schedule(), &Budgeted::new(12));
    let log = link.log();
    assert_eq!(log.iter().filter(|l| l.starts_with("lost:")).count(), 2, "{log:?}");
    assert_eq!(log.iter().filter(|l| l.starts_with("restore:true")).count(), 2, "{log:?}");
}

/// Review of W14, finding 10: one failed connect on one probe tore a healthy session down, at the cost
/// of the whole lease bounce and a new epoch. A loss is TWO failed probes in a row.
#[test]
fn one_failed_probe_is_not_a_loss_and_two_in_a_row_are() {
    let one = Script::new(&[true, false, true, true, false, true, true], &[Verdict::Go], &[Ok(())]);
    supervise(&one, &schedule(), &Budgeted::new(7));
    assert!(
        one.log().iter().all(|l| !l.starts_with("lost:")),
        "isolated failures are forgiven: {:?}",
        one.log()
    );

    let two = Script::new(&[true, false, false, true, true], &[Verdict::Go], &[Ok(())]);
    supervise(&two, &schedule(), &Budgeted::new(6));
    let lost = two.log().iter().filter(|l| l.starts_with("lost:")).count();
    assert_eq!(lost, 1, "two failures in a row are one loss: {:?}", two.log());
}

/// The thing behind the link said so itself (a bridge's `ExecLink{up:false}`, which the IBKR bridge sends at its stream's death): that is not a guess to
/// confirm with a second probe.
#[test]
fn a_loss_the_link_reports_itself_is_acted_on_without_waiting_for_a_second_probe() {
    let link =
        Script::new(&[true], &[Verdict::Go], &[Ok(())]).reporting(&[None, Some("bridge: dead")]);
    supervise(&link, &schedule(), &Budgeted::new(4));
    let log = link.log();
    assert_eq!(log.iter().filter(|l| l.starts_with("lost:")).count(), 1, "{log:?}");
    assert!(
        log.iter().any(|l| l == "lost:bridge: dead"),
        "the loss carries the report's words: {log:?}"
    );
}

/// Review of W14, finding 5: a rows READ that failed (the store was locked for a moment) ended the
/// supervision for good, as if the operator had deactivated the account. It is neither: nothing is
/// restored without an answer, the loop keeps going, and the next answer decides.
#[test]
fn an_unreadable_answer_keeps_supervising_and_restores_nothing_until_the_rows_answer() {
    let link = Script::new(
        &[false, false, true],
        &[
            Verdict::Retry("database is locked".into()),
            Verdict::Retry("database is locked".into()),
            Verdict::Go,
        ],
        &[Ok(())],
    );
    supervise(&link, &schedule(), &Budgeted::new(30));
    let log = link.log();
    assert!(log.iter().all(|l| !l.starts_with("gave_up:")), "a read error is not a stop: {log:?}");
    let first_restore = log.iter().position(|l| l.starts_with("restore:")).expect("restored");
    let first_go = log.iter().position(|l| l == "verdict:true").expect("the rows answered");
    assert!(first_go < first_restore, "no restore before an answer: {log:?}");
    assert!(
        log[..first_go].iter().all(|l| !l.starts_with("restore:")),
        "nothing was restored while the rows were unreadable: {log:?}"
    );
}

/// ...and an unreadable store is not read at the probe cadence for ever: the reads back off.
#[test]
fn an_unreadable_store_is_asked_again_with_a_growing_wait_and_a_stop_still_ends_it() {
    let link = Script::new(
        &[false],
        &[Verdict::Retry("busy".into()), Verdict::Stop("account inactive".into())],
        &[Ok(())],
    );
    // Retry waits one probe period (1 s), then the next read comes one pass later; the second Retry doubles.
    let many_retries = Script::new(&[false], &[Verdict::Retry("busy".into())], &[Ok(())]);
    supervise(&many_retries, &schedule(), &Budgeted::new(40));
    let reads = many_retries.log().iter().filter(|l| l.starts_with("verdict:")).count();
    assert!(reads < 15, "40 passes of an unreadable store are far fewer than 40 reads: {reads}");
    assert!(reads >= 4, "but it keeps asking: {reads}");

    supervise(&link, &schedule(), &Budgeted::new(10));
    assert_eq!(link.log().last().map(String::as_str), Some("gave_up:account inactive"));
}

#[test]
fn the_backoff_doubles_to_its_ceiling_and_resets() {
    let mut b = Backoff::new(2 * SEC, 8 * SEC);
    let waits: Vec<u64> = (0..5).map(|_| b.next_wait().as_secs()).collect();
    assert_eq!(waits, [2, 4, 8, 8, 8]);
    b.reset();
    assert_eq!(b.next_wait(), 2 * SEC);
}

#[test]
fn a_schedule_with_a_zero_wait_is_not_sane() {
    assert!(schedule().is_sane());
    assert!(!Schedule { probe_every: Duration::ZERO, ..schedule() }.is_sane());
    assert!(!Schedule { first_wait: Duration::ZERO, ..schedule() }.is_sane());
    assert!(!Schedule { first_wait: 9 * SEC, ..schedule() }.is_sane());
}
