//! The reconnect loop against a scripted link and a pause that records instead of sleeping.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

use super::{
    VenueLinkReconnectLink, VenueLinkReconnectPause, VenueLinkReconnectSchedule,
    VenueLinkReconnectVerdict, supervise,
};

const SEC: Duration = Duration::from_secs(1);

fn schedule() -> VenueLinkReconnectSchedule {
    VenueLinkReconnectSchedule {
        probe_every: SEC,
        first_wait: 2 * SEC,
        max_wait: 8 * SEC,
        probation: Duration::ZERO,
    }
}

/// A link that plays back scripts: `reachable` answers pop from a queue (the last one repeats), as do
/// `verdict` and `restore`. Every call is recorded in order.
struct Script {
    reachable: RefCell<VecDeque<bool>>,
    verdicts: RefCell<VecDeque<VenueLinkReconnectVerdict>>,
    restores: RefCell<VecDeque<Result<(), String>>>,
    reported: RefCell<VecDeque<Option<String>>>,
    /// The venue's calendar, one answer per ask (the last one repeats); empty = never closed.
    closed: RefCell<VecDeque<Option<String>>>,
    /// The session's own up word, one answer per ask (the last one repeats); empty = it cannot tell (`None`).
    ups: RefCell<VecDeque<Option<bool>>>,
    log: RefCell<Vec<String>>,
}

impl Script {
    fn new(
        reachable: &[bool],
        verdicts: &[VenueLinkReconnectVerdict],
        restores: &[Result<(), String>],
    ) -> Self {
        Self {
            reachable: RefCell::new(reachable.iter().copied().collect()),
            verdicts: RefCell::new(verdicts.iter().cloned().collect()),
            restores: RefCell::new(restores.iter().cloned().collect()),
            reported: RefCell::new(VecDeque::new()),
            closed: RefCell::new(VecDeque::new()),
            ups: RefCell::new(VecDeque::new()),
            log: RefCell::new(Vec::new()),
        }
    }

    /// What the thing behind the link says of itself, one answer per UP pass (then nothing).
    fn reporting(self, said: &[Option<&str>]) -> Self {
        *self.reported.borrow_mut() = said.iter().map(|s| s.map(str::to_string)).collect();
        self
    }

    /// What the venue's calendar answers, one per ask before an attempt (the last one repeats).
    fn calendar(self, closed: &[Option<&str>]) -> Self {
        *self.closed.borrow_mut() = closed.iter().map(|s| s.map(str::to_string)).collect();
        self
    }

    /// What the session says of being up, one answer per ask (the last one repeats).
    fn saying_up(self, ups: &[Option<bool>]) -> Self {
        *self.ups.borrow_mut() = ups.iter().copied().collect();
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

impl VenueLinkReconnectLink for Script {
    fn verdict(&self) -> VenueLinkReconnectVerdict {
        let v = Self::pop(&self.verdicts);
        let word = match &v {
            VenueLinkReconnectVerdict::Go => "true",
            VenueLinkReconnectVerdict::Stop(_) => "false",
            VenueLinkReconnectVerdict::Retry(_) => "retry",
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
    fn closed(&self) -> Option<String> {
        if self.closed.borrow().is_empty() {
            return None;
        }
        let closed = Self::pop(&self.closed);
        self.log.borrow_mut().push(format!("closed:{}", closed.as_deref().unwrap_or("open")));
        closed
    }
    fn held(&self, why: &str) {
        self.log.borrow_mut().push(format!("held:{why}"));
    }
    fn refused(&self, why: &str) {
        self.log.borrow_mut().push(format!("refused:{why}"));
    }
    fn up(&self) -> Option<bool> {
        if self.ups.borrow().is_empty() {
            return None;
        }
        let up = Self::pop(&self.ups);
        self.log.borrow_mut().push(format!("up:{up:?}"));
        up
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

impl VenueLinkReconnectPause for Budgeted {
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
    let link = Script::new(
        &[true, true, false, false, true, true],
        &[VenueLinkReconnectVerdict::Go],
        &[Ok(())],
    );
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
        &[
            VenueLinkReconnectVerdict::Go,
            VenueLinkReconnectVerdict::Go,
            VenueLinkReconnectVerdict::Stop("account inactive".into()),
        ],
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
        &[
            VenueLinkReconnectVerdict::Go,
            VenueLinkReconnectVerdict::Go,
            VenueLinkReconnectVerdict::Stop("deactivated".into()),
        ],
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
        &[VenueLinkReconnectVerdict::Go],
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

/// Grants `budget` pauses and writes each into the link's log as `pause`, so a gap is counted in passes.
struct Logged<'a> {
    link: &'a Script,
    budget: Cell<usize>,
}

impl VenueLinkReconnectPause for Logged<'_> {
    fn pause(&self, _wait: Duration) -> bool {
        self.link.log.borrow_mut().push("pause".into());
        if self.budget.get() == 0 {
            return false;
        }
        self.budget.set(self.budget.get() - 1);
        true
    }
}

/// The passes (pauses) between consecutive `log` lines that start with `prefix`.
fn passes_between(log: &[String], prefix: &str) -> Vec<usize> {
    let mut gaps = Vec::new();
    let mut since: Option<usize> = None;
    for line in log {
        if line.starts_with(prefix) {
            if let Some(n) = since {
                gaps.push(n);
            }
            since = Some(0);
        } else if line == "pause"
            && let Some(n) = since.as_mut()
        {
            *n += 1;
        }
    }
    gaps
}

/// **The supervisor's waits, pinned verbatim before its doubling moved onto the one backoff** (connection plan B3), in
/// passes of the 1 s probe. Six failed restores wait 2, 4, 8, 8, 8 and 8 s (2 s doubling to the 8 s ceiling); the
/// restore that worked starts it over, so the next loss is attempted at once and its two failures wait 2 and 4 s. A
/// store that cannot be read is asked again 1, 2, 4 s later (the probe period, doubling); an answer starts that over,
/// so the next unreadable read waits 1 s, then 2 s.
#[test]
fn the_supervisors_waits_are_pinned_verbatim() {
    let restores = [
        Err("a".into()),
        Err("b".into()),
        Err("c".into()),
        Err("d".into()),
        Err("e".into()),
        Err("f".into()),
        Ok(()),
        Err("g".into()),
        Err("h".into()),
        Ok(()),
    ];
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &restores)
        .reporting(&[Some("one"), Some("two")]);
    supervise(&link, &schedule(), &Logged { link: &link, budget: Cell::new(60) });
    let log = link.log();
    assert_eq!(count(&log, "lost:"), 2, "{log:?}");
    assert_eq!(passes_between(&log, "restore:"), [2, 4, 8, 8, 8, 8, 1, 2, 4], "{log:?}");

    let retry = || VenueLinkReconnectVerdict::Retry("busy".into());
    let verdicts = [
        retry(),
        retry(),
        retry(),
        VenueLinkReconnectVerdict::Go,
        retry(),
        retry(),
        VenueLinkReconnectVerdict::Stop("done".into()),
    ];
    let link = Script::new(&[true], &verdicts, &[Err("refused".into())]).reporting(&[Some("gone")]);
    supervise(&link, &schedule(), &Logged { link: &link, budget: Cell::new(60) });
    let log = link.log();
    assert_eq!(passes_between(&log, "verdict:"), [1, 2, 4, 1, 1, 2], "{log:?}");
    assert_eq!(log.last().map(String::as_str), Some("gave_up:done"));
}

/// **A server that accepts each login and drops it before the session says it is up CLIMBS the backoff** (the FXCM
/// flap: its start returns before the login, so every restore returned `Ok` and the backoff started over, a rebuild
/// every 2-4 s for ever). Pinned in passes of the 1 s probe: the first loss is restored at once (1 pass, as before); each
/// loss of a restore that never said up is one more failed restore, so the next attempt comes one probe plus 2, 4, 8 and
/// 8 s (the ceiling) later: 3, 5, 9, 9 passes. Once a restored session says it is up the backoff starts over: its loss is
/// restored at once (2 passes: the healthy probe, the lost one) and the next flap waits 2 s again (3 passes).
#[test]
fn a_session_that_drops_before_it_says_up_climbs_the_backoff() {
    let lost = Some("the ForexConnect session is SessionLost");
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[lost, lost, lost, lost, lost, None, lost, lost])
        .saying_up(&[
            Some(false),
            Some(false),
            Some(false),
            Some(false),
            Some(false),
            Some(true),
            Some(false),
        ]);
    supervise(&link, &schedule(), &Logged { link: &link, budget: Cell::new(40) });
    let log = link.log();
    assert_eq!(count(&log, "restore:"), 7, "{log:?}");
    assert_eq!(passes_between(&log, "restore:"), [3, 5, 9, 9, 2, 3], "{log:?}");

    // A link that cannot say it is up (`None`, IBKR) is healthy at the restore: the same flap restores at once each time.
    let blind = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[lost, lost, lost, lost]);
    supervise(&blind, &schedule(), &Logged { link: &blind, budget: Cell::new(10) });
    assert_eq!(passes_between(&blind.log(), "restore:"), [1, 1, 1], "{:?}", blind.log());
}

/// The schedule with a probation of three probes (FXCM's is 30 s of 2 s probes: the owner's ruling of 2026-11-22).
fn on_probation() -> VenueLinkReconnectSchedule {
    VenueLinkReconnectSchedule { probation: 3 * SEC, ..schedule() }
}

/// **A session seen up for LESS than the probation and then lost climbs the backoff** (the reviewer's finding on #3345:
/// one sighting of `Up` started the ladder over, so a server that held each session for one probe and dropped it was
/// re-logged-in every few seconds for ever). In passes of the 1 s probe, a 3 s probation: the first flap waits 2 s
/// (3 passes); a session then up for 0, 1 and 2 s and lost is one more failed restore, so the next attempt waits 4 s
/// (8 passes: four UP, four DOWN), and one up for 0 and 1 s waits 8 s (11 passes).
#[test]
fn a_session_up_for_less_than_the_probation_climbs_the_backoff() {
    let lost = Some("the ForexConnect session is SessionLost");
    let (t, f) = (Some(true), Some(false));
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[lost, lost, None, None, None, lost, None, None, lost])
        .saying_up(&[f, f, t, t, t, f, t, t, f]);
    supervise(&link, &on_probation(), &Logged { link: &link, budget: Cell::new(30) });
    let log = link.log();
    assert_eq!(passes_between(&log, "restore:"), [3, 8, 11], "{log:?}");
}

/// **A session seen up for the WHOLE probation is healthy: the backoff starts over.** In passes: two flaps climb to a 4 s
/// wait (3, then 5 passes); a session then up for 0, 1, 2 and 3 s is healthy at the fourth sighting, so its loss is
/// restored at once (5 passes) and the next flap waits the first step again, 2 s (3 passes). A link that cannot say it is
/// up (`None`, IBKR) is healthy at the restore whatever the probation.
#[test]
fn a_session_up_for_the_probation_starts_the_backoff_over() {
    let lost = Some("the ForexConnect session is SessionLost");
    let (t, f) = (Some(true), Some(false));
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[lost, lost, lost, None, None, None, None, lost, lost])
        .saying_up(&[f, f, f, t, t, t, t, f]);
    supervise(&link, &on_probation(), &Logged { link: &link, budget: Cell::new(30) });
    let log = link.log();
    assert_eq!(passes_between(&log, "restore:"), [3, 5, 5, 3], "{log:?}");

    let blind = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[lost, lost, lost, lost]);
    supervise(&blind, &on_probation(), &Logged { link: &blind, budget: Cell::new(10) });
    assert_eq!(passes_between(&blind.log(), "restore:"), [1, 1, 1], "{:?}", blind.log());
}

/// **A pass that does not see the session up starts the probation over.** In passes, a 3 s probation: up for 0 and 1 s,
/// a pass not up, then up for 0, 1 and 2 s and lost: still on probation (five sightings, never three seconds without a
/// break), so the loss waits 2 s (9 passes: seven UP, two DOWN). A span unbroken from 0 to 3 s is healthy, and its loss
/// is restored at once (5 passes).
#[test]
fn a_pass_not_up_starts_the_probation_over() {
    let lost = Some("the ForexConnect session is SessionLost");
    let (t, f) = (Some(true), Some(false));
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[lost, None, None, None, None, None, None, lost, None, None, None, None, lost])
        .saying_up(&[f, t, t, f, t, t, t, f, t, t, t, t, f]);
    supervise(&link, &on_probation(), &Logged { link: &link, budget: Cell::new(20) });
    let log = link.log();
    assert_eq!(passes_between(&log, "restore:"), [9, 5], "{log:?}");
}

#[test]
fn a_pause_that_says_stop_ends_the_loop_in_either_state() {
    let up = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())]);
    supervise(&up, &schedule(), &Budgeted::new(0));
    assert_eq!(up.log(), Vec::<String>::new(), "stopped before the first probe");

    let down = Script::new(&[false], &[VenueLinkReconnectVerdict::Go], &[Ok(())]);
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
        &[VenueLinkReconnectVerdict::Go],
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
    let one = Script::new(
        &[true, false, true, true, false, true, true],
        &[VenueLinkReconnectVerdict::Go],
        &[Ok(())],
    );
    supervise(&one, &schedule(), &Budgeted::new(7));
    assert!(
        one.log().iter().all(|l| !l.starts_with("lost:")),
        "isolated failures are forgiven: {:?}",
        one.log()
    );

    let two =
        Script::new(&[true, false, false, true, true], &[VenueLinkReconnectVerdict::Go], &[Ok(())]);
    supervise(&two, &schedule(), &Budgeted::new(6));
    let lost = two.log().iter().filter(|l| l.starts_with("lost:")).count();
    assert_eq!(lost, 1, "two failures in a row are one loss: {:?}", two.log());
}

/// The thing behind the link said so itself (a bridge's `ExecLink{up:false}`, which the IBKR bridge sends at its stream's death): that is not a guess to
/// confirm with a second probe.
#[test]
fn a_loss_the_link_reports_itself_is_acted_on_without_waiting_for_a_second_probe() {
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[None, Some("bridge: dead")]);
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
            VenueLinkReconnectVerdict::Retry("database is locked".into()),
            VenueLinkReconnectVerdict::Retry("database is locked".into()),
            VenueLinkReconnectVerdict::Go,
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
        &[
            VenueLinkReconnectVerdict::Retry("busy".into()),
            VenueLinkReconnectVerdict::Stop("account inactive".into()),
        ],
        &[Ok(())],
    );
    // Retry waits one probe period (1 s), then the next read comes one pass later; the second Retry doubles.
    let many_retries =
        Script::new(&[false], &[VenueLinkReconnectVerdict::Retry("busy".into())], &[Ok(())]);
    supervise(&many_retries, &schedule(), &Budgeted::new(40));
    let reads = many_retries.log().iter().filter(|l| l.starts_with("verdict:")).count();
    assert!(reads < 15, "40 passes of an unreadable store are far fewer than 40 reads: {reads}");
    assert!(reads >= 4, "but it keeps asking: {reads}");

    supervise(&link, &schedule(), &Budgeted::new(10));
    assert_eq!(link.log().last().map(String::as_str), Some("gave_up:account inactive"));
}

const CLOSED: &str = "FX market closed: login at 2026-11-22T21:00:00Z";

/// The lines of `log` that start with `prefix`.
fn count(log: &[String], prefix: &str) -> usize {
    log.iter().filter(|l| l.starts_with(prefix)).count()
}

/// **A loss while the venue's calendar is closed (the FX weekend) is not a failure**: no attempt is made while it is
/// closed, the hold is said ONCE, the rows are still read on every pass, and the first pass the market is open makes the
/// one attempt.
#[test]
fn a_loss_while_the_market_is_closed_holds_every_attempt_says_so_once_and_logs_in_once_at_the_open()
{
    let link = Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())])
        .reporting(&[Some("SessionLost")])
        .calendar(&[Some(CLOSED), Some(CLOSED), Some(CLOSED), None]);
    supervise(&link, &schedule(), &Budgeted::new(8));
    let log = link.log();
    assert_eq!(count(&log, "lost:"), 1, "{log:?}");
    assert_eq!(count(&log, "held:"), 1, "one line for the whole hold: {log:?}");
    assert!(log.iter().any(|l| *l == format!("held:{CLOSED}")), "the hold names the open: {log:?}");
    let open = log.iter().position(|l| l == "closed:open").expect("the market opened");
    assert_eq!(count(&log[..open], "restore:"), 0, "nothing before the open: {log:?}");
    assert_eq!(
        count(&log[..open], "reachable:"),
        0,
        "not even a probe while it is closed: {log:?}"
    );
    assert_eq!(
        count(&log[..open], "verdict:"),
        4,
        "the rows are read on every pass of the hold: {log:?}"
    );
    assert_eq!(
        &log[open..open + 3],
        ["closed:open", "reachable:true", "restore:true"],
        "the first open pass makes the one attempt: {log:?}"
    );
    assert_eq!(count(&log, "restore:"), 1, "{log:?}");
}

/// **At the open there is one login, then the normal backoff**: a hold starts the backoff over, so a restore that fails
/// after the open waits the FIRST wait (2 s here), however far it had grown before the close. Only the failure AFTER the
/// hold is told to the link (`refused`, so its words replace the held ones); the one before it is not.
#[test]
fn a_hold_starts_the_backoff_over_and_a_failure_after_the_open_waits_the_first_wait() {
    let link = Script::new(
        &[true],
        &[VenueLinkReconnectVerdict::Go],
        &[Err("a".into()), Err("b".into()), Ok(())],
    )
    .reporting(&[Some("SessionLost")])
    // Open (attempt a fails, 2 s), then closed for two asks, then open for good.
    .calendar(&[None, Some(CLOSED), Some(CLOSED), None]);
    supervise(&link, &schedule(), &Budgeted::new(14));
    let log = link.log();
    let restores: Vec<usize> =
        log.iter().enumerate().filter(|(_, l)| l.starts_with("restore:")).map(|(i, _)| i).collect();
    assert_eq!(restores.len(), 3, "{log:?}");
    assert!(
        log[restores[0]..restores[1]].iter().any(|l| l.starts_with("held:")),
        "the close came between the first two attempts: {log:?}"
    );
    let passes = count(&log[restores[1]..restores[2]], "verdict:");
    assert_eq!(passes, 2, "the failure after the open waits the first wait, not twice it: {log:?}");
    assert_eq!(
        log.iter().filter(|l| l.starts_with("refused:")).collect::<Vec<_>>(),
        ["refused:b"],
        "{log:?}"
    );
}

/// **The rows still decide during a hold**: an account deactivated over the weekend is given up, and nothing was attempted.
#[test]
fn the_rows_saying_stop_end_a_hold_with_no_attempt() {
    let link = Script::new(
        &[true],
        &[
            VenueLinkReconnectVerdict::Go,
            VenueLinkReconnectVerdict::Go,
            VenueLinkReconnectVerdict::Stop("deactivated".into()),
        ],
        &[Ok(())],
    )
    .reporting(&[Some("SessionLost")])
    .calendar(&[Some(CLOSED)]);
    supervise(&link, &schedule(), &Budgeted::new(10));
    let log = link.log();
    assert_eq!(count(&log, "restore:"), 0, "{log:?}");
    assert_eq!(log.last().map(String::as_str), Some("gave_up:deactivated"));
}

/// **The calendar is asked only to hold a REBUILD**: a session that is up when the market closes runs on (the SDK keeps it
/// or gives it up; only a loss is acted on).
#[test]
fn a_closed_calendar_takes_no_session_down() {
    let link =
        Script::new(&[true], &[VenueLinkReconnectVerdict::Go], &[Ok(())]).calendar(&[Some(CLOSED)]);
    supervise(&link, &schedule(), &Budgeted::new(6));
    let log = link.log();
    assert!(log.iter().all(|l| l == "reachable:true"), "probes only, no loss, no hold: {log:?}");
}

#[test]
fn a_schedule_with_a_zero_wait_is_not_sane() {
    assert!(schedule().is_sane());
    assert!(!VenueLinkReconnectSchedule { probe_every: Duration::ZERO, ..schedule() }.is_sane());
    assert!(!VenueLinkReconnectSchedule { first_wait: Duration::ZERO, ..schedule() }.is_sane());
    assert!(!VenueLinkReconnectSchedule { first_wait: 9 * SEC, ..schedule() }.is_sane());
}
