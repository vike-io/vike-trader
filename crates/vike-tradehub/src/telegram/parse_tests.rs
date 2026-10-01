use super::*;

#[test]
fn parses_the_write_grammar() {
    let Instruction::Write(cmd) = parse_instruction("/submit hyperliquid BTC buy 0.5 64000") else {
        panic!("expected a submit");
    };
    let WireCommand::Submit(r) = *cmd else { panic!("expected a submit") };
    assert_eq!(
        (r.venue.as_str(), r.symbol.as_str(), r.side, r.qty),
        ("hyperliquid", "BTC", 1, 0.5)
    );
    assert_eq!(r.price, Some(64000.0));
    assert_eq!(r.order_type, "limit");
    assert!(!r.reduce_only);
    assert!(r.client_order_id.is_empty(), "the coid is minted at preview time");

    let Instruction::Write(cmd) =
        parse_instruction("/submit@vike_bot polymarket TOK sell 20 reduce")
    else {
        panic!("expected a submit");
    };
    let WireCommand::Submit(r) = *cmd else { panic!("expected a submit") };
    assert_eq!((r.side, r.order_type.as_str(), r.price), (-1, "market", None));
    assert!(r.reduce_only, "the `reduce` flag rides anywhere after the verb");

    assert_eq!(
        parse_instruction("/cancel abc-1"),
        Instruction::write(WireCommand::Cancel("abc-1".into()))
    );
    assert_eq!(
        parse_instruction("/modify abc-1 qty=3 price=0.5"),
        Instruction::write(WireCommand::Modify {
            client_order_id: "abc-1".into(),
            new_qty: Some(3.0),
            new_price: Some(0.5),
        })
    );
    assert_eq!(
        parse_instruction("/masscancel"),
        Instruction::write(WireCommand::MassCancel { venue: None, symbol: None, account: None })
    );
    assert_eq!(
        parse_instruction("/flatten hyperliquid BTC"),
        Instruction::write(WireCommand::Flatten {
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            account: None
        })
    );
    assert_eq!(
        parse_instruction("/marketexit"),
        Instruction::write(WireCommand::MarketExit { venue: None, account: None })
    );
    assert_eq!(
        parse_instruction("/state halted"),
        Instruction::write(WireCommand::SetTradingState(WireTradingState::Halted))
    );
}

#[test]
fn parses_read_confirm_and_rejects_the_rest() {
    assert_eq!(parse_instruction("/status"), Instruction::Read(ReadVerb::Status));
    assert_eq!(parse_instruction("/POSITIONS"), Instruction::Read(ReadVerb::Positions));
    assert_eq!(parse_instruction("/confirm deadbeef"), Instruction::Confirm("deadbeef".into()));
    assert_eq!(parse_instruction("/help"), Instruction::Help);
    assert_eq!(parse_instruction("/nope"), Instruction::Unknown);
    // No leading slash ⇒ ordinary chatter ⇒ never answered.
    assert_eq!(parse_instruction("hello there"), Instruction::Ignore);
    assert_eq!(parse_instruction("   "), Instruction::Ignore);
    assert!(matches!(parse_instruction("/submit hyperliquid BTC"), Instruction::Malformed(_)));
    assert!(matches!(parse_instruction("/submit v s up 1"), Instruction::Malformed(_)));
    assert!(matches!(parse_instruction("/submit v s buy -1"), Instruction::Malformed(_)));
    assert!(matches!(parse_instruction("/modify abc-1"), Instruction::Malformed(_)));
    assert!(matches!(parse_instruction("/modify abc-1 3"), Instruction::Malformed(_)));
    assert!(matches!(parse_instruction("/state sideways"), Instruction::Malformed(_)));
    assert!(matches!(parse_instruction("/confirm"), Instruction::Malformed(_)));
}

#[test]
fn parses_the_getupdates_payload() {
    let body = serde_json::json!({
        "ok": true,
        "result": [
            {"update_id": 11, "message": {
                "chat": {"id": 42},
                "from": {"id": 7, "username": "alice"},
                "text": "/status"}},
            // A non-text update still yields a row so the ledger can advance past it.
            {"update_id": 12, "message": {
                "chat": {"id": 42}, "from": {"id": 7}, "sticker": {"id": "x"}}},
            // An EDITED message carries no `message` field — never replayed as an instruction,
            // and with no `message` there is no sender either.
            {"update_id": 13, "edited_message": {"chat": {"id": 42}, "text": "/marketexit"}}
        ]
    });
    assert_eq!(
        parse_updates(&body),
        vec![
            TgUpdate {
                update_id: 11,
                chat_id: 42,
                from_id: 7,
                from_username: Some("alice".into()),
                text: "/status".into()
            },
            TgUpdate {
                update_id: 12,
                chat_id: 42,
                from_id: 7,
                from_username: None,
                text: String::new()
            },
            TgUpdate {
                update_id: 13,
                chat_id: 0,
                from_id: UNKNOWN_USER_ID,
                from_username: None,
                text: String::new()
            },
        ]
    );
    assert!(parse_updates(&serde_json::json!({"ok": false})).is_empty());
}

/// ⚠ THE SENDER IS PARSED. Authorization here is per-CHAT, so without `message.from.id` an
/// accepted order in an allowlisted group is attributable only to the room. A `from` that is
/// ABSENT (a channel post, an anonymous group admin) must read as [`UNKNOWN_USER_ID`] — never
/// as something an allowlist could match.
#[test]
fn the_sender_is_parsed_and_an_absent_one_is_unknown() {
    let body = serde_json::json!({
        "result": [
            {"update_id": 1, "message": {
                "chat": {"id": -100777},
                "from": {"id": 55, "username": "bob", "is_bot": false},
                "text": "/marketexit"}},
            // No `from` at all — an anonymous admin or a channel post.
            {"update_id": 2, "message": {"chat": {"id": -100777}, "text": "/marketexit"}},
            // A `from` with no `@handle`, and one with a blank handle: id only, never "@".
            {"update_id": 3, "message": {
                "chat": {"id": -100777}, "from": {"id": 56}, "text": "/status"}},
            {"update_id": 4, "message": {
                "chat": {"id": -100777}, "from": {"id": 57, "username": ""}, "text": "/status"}},
        ]
    });
    let got = parse_updates(&body);
    assert_eq!(
        got.iter().map(|u| u.from_id).collect::<Vec<_>>(),
        vec![55, UNKNOWN_USER_ID, 56, 57]
    );
    assert_eq!(got[0].from_username.as_deref(), Some("bob"));
    for u in &got[1..] {
        assert_eq!(u.from_username, None, "a missing or blank handle is None, never Some(\"\")");
    }
}

#[test]
fn confirm_reason_names_the_origin_the_person_and_keeps_the_literal_text() {
    let reason = confirm_reason(4242, 55, Some("bob"), "/marketexit hyperliquid");
    assert!(reason.contains("telegram chat 4242"), "the audit line must say WHERE: {reason}");
    // …and WHO — the forensic property the chat id alone cannot give in a group.
    assert!(reason.contains("user 55"), "the audit line must say WHO: {reason}");
    assert!(reason.contains("@bob"), "the handle rides along for legibility: {reason}");
    assert!(reason.ends_with("/marketexit hyperliquid"), "verbatim instruction: {reason}");

    // An unattributable sender says so rather than claiming "user 0".
    let anon = confirm_reason(-100777, UNKNOWN_USER_ID, None, "/flatten hyperliquid BTC");
    assert!(anon.contains("user unknown"), "{anon}");
    assert!(!anon.contains("user 0"), "0 must never be rendered as an id: {anon}");
}

/// Preview and confirm are two messages and, in a group, can be two PEOPLE — the token binds to
/// the chat, not the sender. The executed command's audit line therefore names both.
#[test]
fn confirmed_by_appends_the_authorizing_actor_to_the_preview_reason() {
    let previewed = confirm_reason(-100777, 55, Some("bob"), "/marketexit");
    let executed = confirmed_by(&previewed, 66, Some("carol"));
    assert!(executed.starts_with(&previewed), "the preview rationale is kept verbatim");
    assert!(executed.contains("user 55"), "who instructed: {executed}");
    assert!(executed.contains("user 66 (@carol)"), "who authorized: {executed}");
    // …and it is appended unconditionally, so a same-person confirm has the field too.
    assert!(confirmed_by(&previewed, 55, Some("bob")).contains("[/confirm by user 55"));
}

#[test]
fn describe_actor_renders_id_handle_and_unknown() {
    assert_eq!(describe_actor(55, Some("bob")), "user 55 (@bob)");
    assert_eq!(describe_actor(55, None), "user 55");
    assert_eq!(describe_actor(UNKNOWN_USER_ID, None), "user unknown");
}

#[test]
fn fill_coid_only_touches_an_empty_submit() {
    let submit = parse_instruction("/submit v s buy 1 2");
    let Instruction::Write(cmd) = submit else { panic!("expected a write") };
    let WireCommand::Submit(r) = fill_coid(*cmd, "tg-abcd") else { panic!("still a submit") };
    assert_eq!(r.client_order_id, "tg-abcd");
    // A pre-set coid is never overwritten, and a non-submit is untouched.
    let cancel = WireCommand::Cancel("keep".into());
    assert_eq!(fill_coid(cancel.clone(), "tg-zzzz"), cancel);
}

#[test]
fn describe_shows_everything_that_varies() {
    let Instruction::Write(cmd) = parse_instruction("/submit hyperliquid BTC buy 0.5 64000") else {
        panic!("expected a write");
    };
    let line = describe(&fill_coid(*cmd, "tg-1"));
    for needle in ["hyperliquid", "BTC", "buy", "0.5", "64000", "tg-1"] {
        assert!(line.contains(needle), "{needle:?} missing from {line:?}");
    }
}
