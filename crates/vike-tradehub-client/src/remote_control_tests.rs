//! The [`OutcomeBoard`] in isolation — the piece that makes an outcome belong to ONE command.
//! The end-to-end property (refuse one command, accept the next, report each correctly against a
//! REAL node) is `vike-tradehub`'s `a_refusal_is_not_reported_again_for_the_next_accepted_command`;
//! these pin the board's own arms, which that test can only reach one at a time.

use super::*;

const NOW: Duration = Duration::from_millis(0);

fn accepted(coid: &str) -> CommandOutcome {
    CommandOutcome::Accepted { coid: coid.to_string() }
}

/// ⚠ THE DEFECT, at the data-structure level: two commands, two verdicts, and each ticket
/// resolves to ITS OWN. A shared latch answers both with whichever error landed last.
#[test]
fn each_ticket_resolves_to_its_own_verdict() {
    let board = OutcomeBoard::default();
    board.record(0, CommandOutcome::Refused("too big".into()));
    board.record(1, accepted("c-2"));
    assert_eq!(board.wait(0, NOW), Some(CommandOutcome::Refused("too big".into())));
    assert_eq!(
        board.wait(1, NOW),
        Some(accepted("c-2")),
        "the SECOND command was accepted; reporting the first's refusal here is the defect"
    );
    // …and re-reading does not consume: a ticket is idempotent, unlike a taking accessor.
    assert_eq!(board.wait(0, NOW), Some(CommandOutcome::Refused("too big".into())));
}

/// An unresolved ticket is `None` — "not answered yet", which is neither success nor failure.
#[test]
fn an_unanswered_ticket_times_out_as_none() {
    let board = OutcomeBoard::default();
    board.record(0, accepted("c-1"));
    assert_eq!(board.wait(1, Duration::from_millis(20)), None);
}

/// A waiter parked on the condvar is woken by the worker's `record`, not by polling — the
/// reason the caller no longer pays a fixed settle sleep.
#[test]
fn a_waiter_is_woken_when_its_outcome_lands() {
    let board = Arc::new(OutcomeBoard::default());
    let writer = Arc::clone(&board);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        writer.record(7, accepted("late"));
    });
    let started = Instant::now();
    assert_eq!(board.wait(7, Duration::from_secs(5)), Some(accepted("late")));
    assert!(started.elapsed() < Duration::from_secs(4), "woken by the record, not the timeout");
}

/// A dead worker resolves every UNANSWERED ticket at once — a waiter must fail fast rather
/// than burn its whole timeout on a connection that is gone. The terminal answer is the
/// worker's own verdict about what is STILL QUEUED, and the real loop always has one to give
/// ([`CommandOutcome::NeverSent`]: a queued command is by construction one the worker never
/// wrote). The `Disconnected` case is a command the worker had already SENT, and it is filed
/// under its own ticket before finishing — the case below.
#[test]
fn a_dead_worker_resolves_pending_tickets_to_the_terminal_outcome() {
    let never = OutcomeBoard::default();
    never.finish(CommandOutcome::NeverSent);
    assert_eq!(never.wait(0, NOW), Some(CommandOutcome::NeverSent));

    let unknown = OutcomeBoard::default();
    unknown.finish(CommandOutcome::Disconnected);
    assert_eq!(unknown.wait(0, NOW), Some(CommandOutcome::Disconnected));
}

/// ⚠ The IN-FLIGHT command's own verdict beats the terminal one. The worker files a
/// `Disconnected` for the command it wrote and never heard back about, THEN finishes with
/// never-sent for whatever is still queued — so the two never blur into one answer, which is
/// the whole reason the terminal carries an outcome instead of being a flag.
#[test]
fn an_in_flight_commands_own_verdict_beats_the_terminal_one() {
    let board = OutcomeBoard::default();
    board.record(5, CommandOutcome::Disconnected);
    board.finish(CommandOutcome::NeverSent);
    assert_eq!(
        board.wait(5, NOW),
        Some(CommandOutcome::Disconnected),
        "the command that WAS written keeps its unknown outcome — resending it is how one \
             order becomes two"
    );
    assert_eq!(
        board.wait(6, NOW),
        Some(CommandOutcome::NeverSent),
        "…while the one queued behind it never went at all"
    );
}

/// …but a ticket ALREADY answered keeps the node's real verdict, even though the worker has
/// since exited (the ordinary shape: the last command is acked, then the handle is dropped).
#[test]
fn a_resolved_ticket_survives_the_worker_exiting() {
    let board = OutcomeBoard::default();
    board.record(3, accepted("c-3"));
    board.finish(CommandOutcome::NeverSent);
    assert_eq!(board.wait(3, NOW), Some(accepted("c-3")));
    assert_eq!(board.wait(4, NOW), Some(CommandOutcome::NeverSent), "…only unanswered ones");
}

/// The board is BOUNDED: a fire-and-forget caller that never awaits cannot grow it without
/// limit. The newest [`OUTCOME_RETENTION`] answers are kept; older ones fall out.
#[test]
fn the_board_retains_a_bounded_window_of_outcomes() {
    let board = OutcomeBoard::default();
    for seq in 0..(OUTCOME_RETENTION as u64 + 5) {
        board.record(seq, accepted(&format!("c-{seq}")));
    }
    assert_eq!(board.state.lock().expect("board").done.len(), OUTCOME_RETENTION);
    assert_eq!(board.wait(4, NOW), None, "an evicted ticket is simply unanswerable");
    let newest = OUTCOME_RETENTION as u64 + 4;
    assert_eq!(board.wait(newest, NOW), Some(accepted(&format!("c-{newest}"))));
}

/// ⚠ **AN EVICTED TICKET MUST NOT INHERIT `NeverSent`** — the case the test above could not
/// see, because it never lets the worker die. [`OutcomeBoard::wait`] checks `done` and then
/// falls through to `terminal`, and the real worker's `terminal` is
/// [`CommandOutcome::NeverSent`] on every exit path it has ([`OutcomeBoard::finish`] has one
/// call site). So an over-retention ticket resolved, the moment the connection dropped, to
/// "not one byte of it went on the wire, resending is safe" — for a command that had been
/// written, ACKED and executed. `None` is the honest answer and the one
/// [`RemoteControlHandle::await_outcome`] has always documented for this case.
#[test]
fn an_evicted_ticket_is_unknown_not_never_sent_after_the_worker_dies() {
    let board = OutcomeBoard::default();
    for seq in 0..(OUTCOME_RETENTION as u64 + 5) {
        board.record(seq, accepted(&format!("c-{seq}")));
    }
    board.finish(CommandOutcome::NeverSent);

    assert_eq!(
        board.wait(0, NOW),
        None,
        "ticket 0 was ACCEPTED and its answer aged out; answering `NeverSent` here licenses a \
             resend of a command the node executed — one order becoming two"
    );
    // …and the two answers the eviction mark must NOT disturb, one on each side of it.
    let newest = OUTCOME_RETENTION as u64 + 4;
    assert_eq!(
        board.wait(newest, NOW),
        Some(accepted(&format!("c-{newest}"))),
        "an in-retention ticket still reports the node's real verdict"
    );
    assert_eq!(
        board.wait(OUTCOME_RETENTION as u64 + 99, NOW),
        Some(CommandOutcome::NeverSent),
        "…while a ticket ABOVE the mark was never recorded at all — it really was still queued \
             when the worker stopped, which is exactly what the terminal answer is for"
    );
}

/// ⚠ Why the mark is a WATERMARK and cannot be a comparison against `done`'s front. Ticket
/// sequences are minted by `next_seq.fetch_add` BEFORE `try_send`, so under concurrent senders
/// the board is not ordered by sequence at all — here it is filled backwards, which makes the
/// evicted entry the HIGHEST sequence present. A RECORDED answer must win over the mark
/// whatever its sequence, which is what checking `done` first buys; the mark may only ever turn
/// an unrecorded ticket's `NeverSent` into `None`.
#[test]
fn the_eviction_mark_never_overrides_a_recorded_answer() {
    let board = OutcomeBoard::default();
    for seq in (0..OUTCOME_RETENTION as u64).rev() {
        board.record(seq, accepted(&format!("c-{seq}")));
    }
    // One more fills the window and evicts the FRONT — whose sequence is the largest here.
    board.record(1000, accepted("c-1000"));
    board.finish(CommandOutcome::NeverSent);

    let evicted = OUTCOME_RETENTION as u64 - 1;
    assert_eq!(board.wait(evicted, NOW), None, "the entry that actually fell out of the window");
    assert_eq!(
        board.wait(evicted - 1, NOW),
        Some(accepted(&format!("c-{}", evicted - 1))),
        "…while a still-recorded answer BELOW the mark is reported unchanged: a `seq < front` \
             rule would have swallowed this one and every other answer on the board"
    );
    assert_eq!(board.wait(0, NOW), Some(accepted("c-0")));
    assert_eq!(board.wait(1000, NOW), Some(accepted("c-1000")));
}
