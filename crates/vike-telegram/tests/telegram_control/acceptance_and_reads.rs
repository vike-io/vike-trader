//! What a confirm prints for each outcome, the read verbs, and housekeeping — over the scripted deps.
//! The same paths against a real node are `node_wire.rs`'s.

use super::*;
use std::time::Duration;
use vike_telegram::TickReport;
use vike_tradehub_client::refusals::TradehubRefusal;
use vike_tradehub_client::{CommandRefusal, UnansweredCommand};

/// Read verbs answer straight from the node's frame: no token, no confirm, nothing sent. Plain
/// chatter and non-text updates are consumed silently (the offset still advances) so a bot sitting
/// in a busy group is not a noise source.
#[test]
fn read_verbs_answer_without_touching_the_core() {
    let dependencies = StubDependencies::new();
    dependencies.push_batch(vec![
        update(1, CHAT, "/status"),
        update(2, CHAT, "/positions"),
        update(3, CHAT, "/orders"),
        update(4, CHAT, "/equity"),
        update(5, CHAT, "morning"),
        TelegramUpdate {
            update_id: 6,
            chat_id: CHAT,
            from_id: USER,
            from_username: None,
            text: String::new(),
        },
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&dependencies, &config(), &led, &mut pending);

    assert_eq!(report.replies, 4, "only the four read verbs are answered");
    assert!(dependencies.accepted().is_empty());
    assert!(report.previewed.is_empty());
    let texts: Vec<String> = dependencies.replies().into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts, ["read:Status", "read:Positions", "read:Orders", "read:Equity"]);
    assert_eq!(led.last(), 6, "every update, answered or not, advances the offset");
}

/// An unrecognized or malformed `/verb` is answered with the usage line — and nothing else happens.
#[test]
fn unknown_and_malformed_instructions_only_get_usage() {
    let dependencies = StubDependencies::new();
    dependencies.push_batch(vec![
        update(1, CHAT, "/wat"),
        update(2, CHAT, "/submit hyperliquid"),
        update(3, CHAT, "/state sideways"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&dependencies, &config(), &led, &mut pending);

    assert_eq!(report.replies, 3);
    assert!(report.previewed.is_empty() && report.executed.is_empty());
    assert!(dependencies.accepted().is_empty());
    for (_, text) in dependencies.replies() {
        assert!(text.contains("/confirm <token>"), "every answer carries the usage line: {text}");
    }
}

/// A preview the node refuses mints no token and sends nothing, and the reply carries the node's
/// own words.
#[test]
fn a_refused_preview_mints_no_token_and_carries_the_nodes_words() {
    let dependencies = StubDependencies::new().with_preview(Box::new(|_cmd| {
        Some(NotExecuted {
            outcome: FailureOutcome::Refused,
            why: "notional 70000 exceeds the cap 1000".to_string(),
        })
    }));
    dependencies.push_batch(vec![update(1, CHAT, "/submit hyperliquid BTC buy 1 70000")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&dependencies, &config(), &led, &mut pending);

    assert!(report.previewed.is_empty() && pending.is_empty(), "nothing worth confirming");
    assert_eq!(report.refused, vec!["notional 70000 exceeds the cap 1000".to_string()]);
    let text = dependencies.replies().pop().expect("a reply").1;
    assert!(text.starts_with("REFUSED (nothing sent)\nsubmit hyperliquid BTC buy 1"), "{text}");
    assert!(text.ends_with("notional 70000 exceeds the cap 1000"), "{text}");
}

/// A refusal of the SENT command (the node's door) is reported back and the command is gone — a
/// failed confirm is never silently retried behind the operator's back.
#[test]
fn a_refused_confirm_reports_and_drops_the_command() {
    let refusal = CommandRefusal::Text("notional 70000 exceeds the cap 1000".to_string());
    let refused = NotExecuted { outcome: FailureOutcome::Refused, why: refusal.to_string() };
    let (report, pending, last) = confirm_not_executed(refused);
    assert!(report.executed.is_empty() && report.not_now.is_empty(), "{report:?}");
    assert_eq!(report.refused, vec![refusal.to_string()]);
    assert!(pending.is_empty(), "the token was spent even though the node refused");
    assert_eq!(last, format!("REFUSED\ncancel ORDER-A\n{refusal}"));
}

/// Confirm `/cancel ORDER-A` over deps whose node did not execute it (`not_executed`): the report, what is
/// left pending and the chat's last reply.
fn confirm_not_executed(not_executed: NotExecuted) -> (TickReport, PendingConfirms, String) {
    let dependencies = StubDependencies::new()
        .with_accept(Box::new(move |_cmd, _reason| Outcome::NotExecuted(not_executed.clone())));
    dependencies.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let previewed = poll_once(&dependencies, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    dependencies.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&dependencies, &config(), &led, &mut pending);
    (report, pending, dependencies.replies().last().cloned().expect("a reply").1)
}

/// **A node that is busy or still starting reads NOT NOW, never REFUSED** (the control link's worker decides it,
/// from the frame of a node of this build or an older node's words): nothing executed and the same command is taken
/// shortly.
#[test]
fn a_busy_or_starting_node_reads_not_now_never_refused() {
    let busy = CommandRefusal::Frame(TradehubRefusal::Busy.into());
    let starting = CommandRefusal::Text(vike_tradehub_client::proto::starting_error("binance up"));
    for refusal in [busy, starting] {
        let not_now = NotExecuted { outcome: FailureOutcome::NotNow, why: refusal.to_string() };
        let (report, _pending, last) = confirm_not_executed(not_now);
        assert!(report.executed.is_empty() && report.refused.is_empty(), "{report:?}");
        assert_eq!(report.not_now, vec![refusal.to_string()]);
        assert_eq!(last, format!("NOT NOW\ncancel ORDER-A\n{refusal}"));
    }
}

/// **A command stopped before the node (no key, a dial, a full queue, a capability an older node lacks) reads
/// NOT SENT, never REFUSED.**
#[test]
fn a_command_stopped_before_the_node_reads_not_sent_never_refused() {
    let why = "this node does not advertise the capability this command requires";
    let not_sent = NotExecuted { outcome: FailureOutcome::NotSent, why: why.to_string() };
    let (report, _pending, last) = confirm_not_executed(not_sent);
    assert!(report.executed.is_empty() && report.not_now.is_empty(), "{report:?}");
    assert_eq!(report.refused, vec![why.to_string()]);
    assert_eq!(last, format!("NOT SENT\ncancel ORDER-A\n{why}"));
}

/// Confirm `/flatten hyperliquid/LIVE BTC` over deps whose node answers nothing (`unanswered`): the report
/// and the chat's last reply.
fn confirm_unanswered(unanswered: UnansweredCommand) -> (TickReport, String) {
    let dependencies = StubDependencies::new()
        .with_accept(Box::new(move |_cmd, _reason| Outcome::Unanswered(unanswered)));
    dependencies.push_batch(vec![update(1, CHAT, "/flatten hyperliquid/LIVE BTC")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let token = poll_once(&dependencies, &config(), &led, &mut pending).previewed.remove(0);
    dependencies.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&dependencies, &config(), &led, &mut pending);
    (report, dependencies.replies().last().cloned().expect("a reply").1)
}

/// **A command whose answer never came reads NEITHER sent NOR refused.** The process wrote it to the
/// node and the reply was lost, so it may have executed: the reply says OUTCOME UNKNOWN in the one
/// wording the desktop, the CLI and MCP show, and says what to check.
#[test]
fn an_unknown_outcome_is_never_reported_as_sent_or_refused() {
    for unanswered in
        [UnansweredCommand::LinkDropped, UnansweredCommand::NoAnswerWithin(Duration::from_secs(6))]
    {
        let (report, last) = confirm_unanswered(unanswered);
        assert!(report.executed.is_empty() && report.refused.is_empty(), "{report:?}");
        assert_eq!(report.unconfirmed, vec![unanswered.to_string()]);
        assert_eq!(
            last,
            format!(
                "OUTCOME UNKNOWN\nflatten hyperliquid BTC account=LIVE\n{unanswered} Check /orders and /positions."
            )
        );
    }
}

/// A command the node provably never got, even after the one redial, reads NOT SENT in the one wording:
/// nothing executed, so it counts as a refusal and never as an unknown.
#[test]
fn a_command_that_never_left_reads_not_sent() {
    let (report, last) = confirm_unanswered(UnansweredCommand::LinkAlreadyClosed);
    assert!(report.executed.is_empty() && report.unconfirmed.is_empty(), "{report:?}");
    assert_eq!(report.refused, vec![UnansweredCommand::LinkAlreadyClosed.to_string()]);
    assert_eq!(
        last,
        format!(
            "NOT SENT\nflatten hyperliquid BTC account=LIVE\n{}",
            UnansweredCommand::LinkAlreadyClosed
        )
    );
}
