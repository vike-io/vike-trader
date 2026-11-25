//! The [`CommandOutcomeBoard`] in isolation: the piece that makes an outcome belong to ONE command. The
//! end-to-end property against a REAL node is `vike-tradehub`'s
//! `a_refusal_is_not_reported_again_for_the_next_accepted_command`; these pin the board's arms.

use super::*;
use std::sync::Arc;

const NOW: Duration = Duration::from_millis(0);

fn accepted(order_client_id: &str) -> CommandOutcome {
    CommandOutcome::Accepted { order_client_id: order_client_id.to_string(), notes: Vec::new() }
}

/// A node's final no in its own words (a `Response::Error`).
fn refused(text: &str) -> CommandOutcome {
    CommandOutcome::Refused(CommandRefusal::Text(text.to_string()))
}

/// ⚠ THE DEFECT at the data-structure level: two commands, two verdicts, each ticket resolves to
/// ITS OWN. A shared latch answers both with whichever error landed last.
#[test]
fn each_ticket_resolves_to_its_own_verdict() {
    let board = CommandOutcomeBoard::default();
    board.record(0, refused("too big"));
    board.record(1, accepted("c-2"));
    assert_eq!(board.wait(0, NOW), Some(refused("too big")));
    assert_eq!(
        board.wait(1, NOW),
        Some(accepted("c-2")),
        "the SECOND command was accepted; reporting the first's refusal here is the defect"
    );
    // …and re-reading does not consume: a ticket is idempotent, unlike a taking accessor.
    assert_eq!(board.wait(0, NOW), Some(refused("too big")));
}

/// An unresolved ticket is `None`: "not answered yet", neither success nor failure.
#[test]
fn an_unanswered_ticket_times_out_as_none() {
    let board = CommandOutcomeBoard::default();
    board.record(0, accepted("c-1"));
    assert_eq!(board.wait(1, Duration::from_millis(20)), None);
}

/// A parked waiter is woken by the worker's `record`, not by a fixed settle sleep.
#[test]
fn a_waiter_is_woken_when_its_outcome_lands() {
    let board = Arc::new(CommandOutcomeBoard::default());
    let writer = Arc::clone(&board);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        writer.record(7, accepted("late"));
    });
    let started = Instant::now();
    assert_eq!(board.wait(7, Duration::from_secs(5)), Some(accepted("late")));
    assert!(started.elapsed() < Duration::from_secs(4), "woken by the record, not the timeout");
}

/// A dead worker resolves every UNANSWERED ticket at once to its terminal verdict, so a waiter
/// fails fast. The real loop's terminal is [`CommandOutcome::NeverSent`] (a queued command was
/// never written); a command already SENT is filed under its own ticket first (next test).
#[test]
fn a_dead_worker_resolves_pending_tickets_to_the_terminal_outcome() {
    let never = CommandOutcomeBoard::default();
    never.finish(CommandOutcome::NeverSent);
    assert_eq!(never.wait(0, NOW), Some(CommandOutcome::NeverSent));

    let unknown = CommandOutcomeBoard::default();
    unknown.finish(CommandOutcome::Disconnected);
    assert_eq!(unknown.wait(0, NOW), Some(CommandOutcome::Disconnected));
}

/// ⚠ The IN-FLIGHT command's own `Disconnected` beats the terminal never-sent: the reason the
/// terminal carries an outcome instead of being a flag.
#[test]
fn an_in_flight_commands_own_verdict_beats_the_terminal_one() {
    let board = CommandOutcomeBoard::default();
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

/// A ticket ALREADY answered keeps the node's real verdict after the worker exits (the ordinary
/// shape: the last command is acked, then the handle is dropped).
#[test]
fn a_resolved_ticket_survives_the_worker_exiting() {
    let board = CommandOutcomeBoard::default();
    board.record(3, accepted("c-3"));
    board.finish(CommandOutcome::NeverSent);
    assert_eq!(board.wait(3, NOW), Some(accepted("c-3")));
    assert_eq!(board.wait(4, NOW), Some(CommandOutcome::NeverSent), "…only unanswered ones");
}

/// The board is BOUNDED: the newest [`OUTCOME_RETENTION`] answers are kept, older ones fall out.
#[test]
fn the_board_retains_a_bounded_window_of_outcomes() {
    let board = CommandOutcomeBoard::default();
    for seq in 0..(OUTCOME_RETENTION as u64 + 5) {
        board.record(seq, accepted(&format!("c-{seq}")));
    }
    assert_eq!(board.state.lock().expect("board").done.len(), OUTCOME_RETENTION);
    assert_eq!(board.wait(4, NOW), None, "an evicted ticket is simply unanswerable");
    let newest = OUTCOME_RETENTION as u64 + 4;
    assert_eq!(board.wait(newest, NOW), Some(accepted(&format!("c-{newest}"))));
}

/// ⚠ **AN EVICTED TICKET MUST NOT INHERIT `NeverSent`** once the worker dies: an over-retention
/// ticket used to fall through to `terminal` and answer "not one byte went, resending is safe" for
/// a command written, ACKED and executed. `None` is the honest answer.
#[test]
fn an_evicted_ticket_is_unknown_not_never_sent_after_the_worker_dies() {
    let board = CommandOutcomeBoard::default();
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

/// ⚠ Why the mark is a WATERMARK, not a comparison against `done`'s front: under concurrent
/// senders the board is not ordered by sequence. Filled backwards here, so the evicted entry is
/// the HIGHEST sequence present; a RECORDED answer must still win whatever its sequence.
#[test]
fn the_eviction_mark_never_overrides_a_recorded_answer() {
    let board = CommandOutcomeBoard::default();
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

/// The unanswered cases in the four failure words: a dropped link and a wait that ran out are OUTCOME UNKNOWN
/// (never resent under a new id), a link already closed is NOT SENT, and an answer is no unanswered case.
#[test]
fn an_unanswered_command_is_unknown_unless_nothing_left_this_client() {
    use vike_utility::errors::FailureOutcome;
    assert_eq!(CommandOutcome::Disconnected.unanswered(), Some(UnansweredCommand::LinkDropped));
    assert_eq!(CommandOutcome::NeverSent.unanswered(), Some(UnansweredCommand::LinkAlreadyClosed));
    assert_eq!(accepted("c-1").unanswered(), None);
    assert_eq!(refused("no").unanswered(), None);
    assert_eq!(
        CommandOutcome::NotNow(CommandRefusal::Text("starting: x".into())).unanswered(),
        None
    );
    for (unanswered, outcome) in [
        (UnansweredCommand::LinkDropped, FailureOutcome::OutcomeUnknown),
        (UnansweredCommand::NoAnswerWithin(Duration::from_secs(6)), FailureOutcome::OutcomeUnknown),
        (UnansweredCommand::LinkAlreadyClosed, FailureOutcome::NotSent),
    ] {
        assert_eq!(unanswered.failure_outcome(), outcome, "{unanswered:?}");
    }
}

/// **A starting node or a busy core is NOT NOW, decided once by the frame's id or, from an older node, by its
/// words; every other refusal is a final no.** The frame's words carry its id; NOT NOW says so in the one wording.
#[test]
fn a_refusal_is_not_now_only_for_a_starting_node_or_a_busy_core() {
    use crate::proto::{BUSY_ERROR, starting_error};
    use crate::refusals::TradehubRefusal;
    let busy = CommandRefusal::Frame(TradehubRefusal::Busy.into());
    let starting =
        CommandRefusal::Frame(TradehubRefusal::Starting { progress: "binance up".into() }.into());
    for (refusal, not_now) in [
        (busy.clone(), true),
        (starting, true),
        // An older node: no frame, its starting and busy words.
        (CommandRefusal::Text(BUSY_ERROR.into()), true),
        (CommandRefusal::Text(starting_error("binance up")), true),
        (CommandRefusal::Text("notional over the cap".into()), false),
        // An id this build does not know is the node's answer, whatever its words.
        (CommandRefusal::Frame(HubRefusal::new("tradehub.newer", BUSY_ERROR)), false),
    ] {
        let decided = if not_now {
            CommandOutcome::NotNow(refusal.clone())
        } else {
            CommandOutcome::Refused(refusal.clone())
        };
        assert_eq!(CommandOutcome::of_refusal(refusal), decided);
    }
    assert_eq!(
        busy.to_string(),
        "NOT NOW (error [tradehub.busy]: core busy, retry): nothing was executed; send the same \
         command again shortly."
    );
    assert_eq!(CommandRefusal::Text("no".into()).to_string(), "no");
    let newer = CommandRefusal::Frame(HubRefusal::new("tradehub.newer", "no"));
    assert_eq!(newer.to_string(), "error [tradehub.newer]: no");
}

/// **A command this client did not queue has ONE wording on every surface, NOT SENT**: a closed link says what a
/// never-sent command says, word for word.
#[test]
fn a_command_not_queued_is_not_sent_in_one_wording() {
    assert_eq!(
        ControlRejected::Busy.to_string(),
        "NOT SENT: the queue to the node was full, so nothing of this command was enqueued and \
         nothing executed. Send it again shortly."
    );
    assert_eq!(ControlRejected::Gone.to_string(), UnansweredCommand::LinkAlreadyClosed.to_string());
    assert_eq!(
        ControlRejected::UnsupportedByHub.to_string(),
        "NOT SENT: this node does not advertise the capability this command requires, so it was \
         refused CLIENT-side and nothing went on the wire."
    );
}
