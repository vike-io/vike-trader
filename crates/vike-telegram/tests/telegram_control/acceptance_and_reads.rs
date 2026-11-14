//! What a confirm prints for each outcome, the read verbs, and housekeeping — over the scripted deps.
//! The same paths against a real node are `node_wire.rs`'s.

use super::*;

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
    let dependencies = StubDependencies::new()
        .with_preview(Box::new(|_cmd| Some("notional 70000 exceeds the cap 1000".to_string())));
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
    let dependencies = StubDependencies::new()
        .with_accept(Box::new(|_cmd, _reason| Outcome::Refused("core busy, retry".to_string())));
    dependencies.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let previewed = poll_once(&dependencies, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    dependencies.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&dependencies, &config(), &led, &mut pending);

    assert!(report.executed.is_empty());
    assert_eq!(report.refused, vec!["core busy, retry".to_string()]);
    assert!(pending.is_empty(), "the token was spent even though the node refused");
    let last = dependencies.replies().last().cloned().expect("a reply").1;
    assert!(last.contains("REFUSED") && last.contains("core busy"), "{last}");
}

/// **A command whose answer never came reads NEITHER sent NOR refused.** The process wrote it to the
/// node and the reply was lost, so it may have executed: the reply says so and says what to check.
#[test]
fn an_unknown_outcome_is_never_reported_as_sent_or_refused() {
    let dependencies = StubDependencies::new().with_accept(Box::new(|_cmd, _reason| {
        Outcome::Unknown("the control connection dropped before the node answered".to_string())
    }));
    dependencies.push_batch(vec![update(1, CHAT, "/flatten hyperliquid/LIVE BTC")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let token = poll_once(&dependencies, &config(), &led, &mut pending).previewed.remove(0);
    dependencies.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&dependencies, &config(), &led, &mut pending);

    assert!(report.executed.is_empty() && report.refused.is_empty(), "{report:?}");
    assert_eq!(report.unconfirmed.len(), 1);
    let last = dependencies.replies().last().cloned().expect("a reply").1;
    assert!(last.starts_with("NOT CONFIRMED\nflatten hyperliquid BTC account=LIVE\n"), "{last}");
    assert!(!last.contains("SENT") && !last.contains("REFUSED"), "{last}");
}
