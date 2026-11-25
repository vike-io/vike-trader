use super::*;
use std::assert_matches;

/// The instruction a write line parses to, spelled as the command it sends.
fn wrote(cmd: CommandWire) -> Instruction {
    Instruction::Write(Box::new(cmd))
}

#[test]
fn parses_the_write_grammar() {
    let Instruction::Write(cmd) = parse_instruction("/submit hyperliquid BTC buy 0.5 64000") else {
        panic!("expected a submit");
    };
    let CommandWire::Submit(r) = *cmd else { panic!("expected a submit") };
    assert_eq!(
        (r.venue.as_str(), r.symbol.as_str(), r.side, r.qty),
        ("hyperliquid", "BTC", 1, 0.5)
    );
    assert_eq!(r.price, Some(64000.0));
    assert_eq!(r.order_type, "limit");
    assert!(!r.reduce_only);
    assert!(r.order_client_id.is_empty(), "the order client id is minted at preview time");

    let Instruction::Write(cmd) =
        parse_instruction("/submit@vike_bot polymarket TOK sell 20 reduce")
    else {
        panic!("expected a submit");
    };
    let CommandWire::Submit(r) = *cmd else { panic!("expected a submit") };
    assert_eq!((r.side, r.order_type.as_str(), r.price), (-1, "market", None));
    assert!(r.reduce_only, "the `reduce` flag rides anywhere after the verb");

    assert_eq!(parse_instruction("/cancel abc-1"), wrote(CommandWire::Cancel("abc-1".into())));
    assert_eq!(
        parse_instruction("/modify abc-1 qty=3 price=0.5"),
        wrote(CommandWire::Modify {
            order_client_id: "abc-1".into(),
            new_qty: Some(3.0),
            new_price: Some(0.5),
            new_trigger_price: None,
        })
    );
    assert_eq!(
        parse_instruction("/masscancel"),
        wrote(CommandWire::MassCancel { venue: None, symbol: None, account: None })
    );
    assert_eq!(
        parse_instruction("/flatten hyperliquid BTC"),
        wrote(CommandWire::Flatten {
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            account: None
        })
    );
    assert_eq!(
        parse_instruction("/closeall"),
        wrote(CommandWire::MarketExit { venue: None, account: None })
    );
    assert_eq!(
        parse_instruction("/state halted"),
        wrote(CommandWire::SetTradingState(TradingState::Halted))
    );
}

/// `/state` takes the operator's word in any case, refuses anything else with the sentence it always
/// printed, and the usage line lists exactly the model's words (`USAGE` is a `const`, so it spells them).
#[test]
fn the_state_verb_reads_the_trading_state_words() {
    for (line, state) in [
        ("/state active", TradingState::Active),
        ("/state Reducing", TradingState::Reducing),
        ("/state HALTED", TradingState::Halted),
    ] {
        assert_eq!(parse_instruction(line), wrote(CommandWire::SetTradingState(state)), "{line}");
    }
    for bad in ["/state", "/state halt", "/state off"] {
        assert_eq!(
            parse_instruction(bad),
            Instruction::Malformed("/state needs one of: active | reducing | halted".into()),
            "{bad}"
        );
    }
    let usage = format!("/state <{}>", TradingState::WORDS.join("|"));
    assert!(crate::USAGE.contains(&usage), "USAGE must list {usage}");
}

/// `/resumevenue` is the Telegram spelling of the per-venue resume (decision 0180): a venue, and the
/// override only as the bare word `force`. Anything else is a usage error, never a silent override.
#[test]
fn parses_the_per_venue_resume_and_spells_the_override_only_as_force() {
    assert_eq!(
        parse_instruction("/resumevenue bybit"),
        wrote(CommandWire::ResumeVenue { venue: "bybit".into(), force: false })
    );
    assert_eq!(
        parse_instruction("/resumevenue@vike_bot bybit FORCE"),
        wrote(CommandWire::ResumeVenue { venue: "bybit".into(), force: true })
    );
    for bad in [
        "/resumevenue",
        "/resumevenue bybit please",
        "/resumevenue bybit --force",
        "/resumevenue bybit force now",
    ] {
        assert_matches!(parse_instruction(bad), Instruction::Malformed(_), "{bad}");
    }
    assert!(crate::USAGE.contains("/resumevenue"), "the usage names the verb");
    let forced = describe(&CommandWire::ResumeVenue { venue: "bybit".into(), force: true });
    assert!(forced.contains("bybit") && forced.contains("FORCE"), "{forced}");
    let plain = describe(&CommandWire::ResumeVenue { venue: "bybit".into(), force: false });
    assert!(plain.contains("bybit") && !plain.contains("FORCE"), "{plain}");
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
    assert_matches!(parse_instruction("/submit hyperliquid BTC"), Instruction::Malformed(_));
    assert_matches!(parse_instruction("/submit v s up 1"), Instruction::Malformed(_));
    assert_matches!(parse_instruction("/submit v s buy -1"), Instruction::Malformed(_));
    assert_matches!(parse_instruction("/modify abc-1"), Instruction::Malformed(_));
    assert_matches!(parse_instruction("/modify abc-1 3"), Instruction::Malformed(_));
    assert_matches!(parse_instruction("/state sideways"), Instruction::Malformed(_));
    assert_matches!(parse_instruction("/confirm"), Instruction::Malformed(_));
}

/// **A command this channel took once is refused naming the new one**, by its id: never read as an
/// unknown word, never an alias, and the usage names only the new one.
#[test]
fn every_renamed_command_is_refused_naming_its_new_word() {
    for &(was, now) in RENAMED_COMMANDS {
        let refusal = TelegramRefusal::Renamed { was, now };
        assert_eq!(refusal.id(), TelegramRefusalId::Renamed);
        let Instruction::Malformed(why) = parse_instruction(&format!("/{was} hyperliquid")) else {
            panic!("/{was} must be refused");
        };
        assert_eq!(why, refusal.rendered());
        assert!(why.starts_with("error [telegram.command.renamed]: "), "{why}");
        assert!(why.contains(&format!("/{now}")), "{why}");
        assert!(crate::USAGE.contains(&format!("/{now} ")), "the usage names /{now}");
        assert!(!crate::USAGE.contains(&format!("/{was}")), "the usage names no /{was}");
    }
}

/// **A phone can name the account the door asks it for** (decision 0254): `/flatten`, `/closeall`,
/// `/masscancel` and `/submit` read `<venue>/<LABEL>` and `<venue>/DEFAULT`, the line the operator confirms says
/// which, and a bare venue names none. No venue stays the node-wide panic button.
#[test]
fn the_venue_token_names_the_account_on_every_verb_that_takes_one() {
    let line = |cmd: &str| match parse_instruction(cmd) {
        Instruction::Write(cmd) => *cmd,
        other => panic!("{cmd}: {other:?}"),
    };
    let flatten = |account: Option<&str>| CommandWire::Flatten {
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        account: account.map(str::to_string),
    };
    assert_eq!(line("/flatten hyperliquid BTC"), flatten(None));
    assert_eq!(line("/flatten hyperliquid/LIVE BTC"), flatten(Some("LIVE")));
    assert_eq!(line("/flatten hyperliquid/DEFAULT BTC"), flatten(Some("DEFAULT")));
    assert_eq!(
        line("/closeall hyperliquid/LIVE"),
        CommandWire::MarketExit { venue: Some("hyperliquid".into()), account: Some("LIVE".into()) }
    );
    assert_eq!(
        line("/masscancel hyperliquid/LIVE BTC"),
        CommandWire::MassCancel {
            venue: Some("hyperliquid".into()),
            symbol: Some("BTC".into()),
            account: Some("LIVE".into())
        }
    );
    let CommandWire::Submit(order) = line("/submit bybit/DEMO BTCUSDT buy 1") else {
        panic!("a submit")
    };
    assert_eq!((order.venue.as_str(), order.account.as_deref()), ("bybit", Some("DEMO")));
    let CommandWire::Submit(bare) = line("/submit bybit BTCUSDT buy 1") else { panic!("a submit") };
    assert_eq!(bare.account, None);
    assert_eq!(
        line("/closeall"),
        CommandWire::MarketExit { venue: None, account: None },
        "the panic button"
    );
    assert!(describe(&flatten(Some("LIVE"))).ends_with(" account=LIVE"));
    assert!(!describe(&flatten(None)).contains("account="));
    for bad in ["/flatten hyperliquid/live BTC", "/closeall /LIVE", "/submit a/b/c X buy 1"] {
        assert_matches!(parse_instruction(bad), Instruction::Malformed(_), "{bad}");
    }
}

/// **A book token reads on a phone as it reads on `vike-cli`.** A route key is what the node prints, so it is
/// what gets copied into a chat, and it used to travel as a venue literally named `binance#ALT`, which the
/// node's door lets through while its roster is empty. A mark did the same.
#[test]
fn a_route_key_or_a_mark_is_refused_on_every_verb_that_takes_a_book() {
    let refused = |cmd: &str| match parse_instruction(cmd) {
        Instruction::Malformed(why) => why,
        other => panic!("{cmd}: {other:?}"),
    };
    for cmd in [
        "/submit binance#ALT BTCUSDT buy 1",
        "/flatten binance#ALT BTCUSDT",
        "/masscancel binance#ALT",
        "/closeall binance#ALT",
    ] {
        let why = refused(cmd);
        assert!(why.contains("ROUTE KEY") && why.contains("`binance/ALT`"), "{cmd}: {why}");
    }
    for cmd in [
        "/submit @hedge BTCUSDT buy 1",
        "/flatten @hedge BTCUSDT",
        "/masscancel @hedge",
        "/closeall @hedge",
    ] {
        assert!(refused(cmd).contains("MARK"), "{cmd}");
    }
}

/// **The line an operator confirms prints an engine number as `vike-cli`'s preview does**, with no binary
/// tail (`8_092_593.0 * 0.01` is `80925.93000000001`, measured on the OKX demo).
#[test]
fn the_preview_line_prints_a_number_without_its_float_tail() {
    let (qty, price) = (0.4 * 3.0, 8_092_593.0_f64 * 0.01);
    assert_eq!(price.to_string(), "80925.93000000001", "the fixture is the measured value");
    assert_eq!(qty.to_string(), "1.2000000000000002");

    let Instruction::Write(cmd) = parse_instruction(&format!("/submit okx S buy {qty} {price}"))
    else {
        panic!("a submit");
    };
    assert_eq!(
        describe(&with_order_client_id(*cmd, "c")),
        "submit okx S buy 1.2 limit @ 80925.93 order_client_id=c"
    );
    assert_eq!(
        describe(&CommandWire::Modify {
            order_client_id: "c".into(),
            new_qty: Some(qty),
            new_price: Some(price),
            new_trigger_price: None
        }),
        "modify c qty=1.2 price=80925.93"
    );
    let bracket = vike_tradehub_client::wire::OrderBracketWire {
        order_client_id: None,
        venue: "okx".into(),
        symbol: "S".into(),
        side: 1,
        qty,
        entry_price: Some(price),
        stop_loss: price,
        take_profit: price,
        account: None,
    };
    assert_eq!(
        describe(&CommandWire::Bracket(bracket)),
        "bracket okx S buy 1.2 entry=limit @ 80925.93 sl=80925.93 tp=80925.93 account=-"
    );
}

/// A `/modify` number that is not finite never reaches the wire, where the JSON frame would write it as
/// `null` and the node would change the other field only (`vike-cli`'s `modify_number` is the CLI's twin).
#[test]
fn a_modify_number_that_is_not_finite_is_malformed() {
    for bad in [
        "/modify abc-1 price=1e999",
        "/modify abc-1 price=inf",
        "/modify abc-1 price=nan",
        "/modify abc-1 qty=1e999",
        "/modify abc-1 qty=inf",
        "/modify abc-1 qty=-inf",
        "/modify abc-1 qty=3 price=nan",
    ] {
        match parse_instruction(bad) {
            Instruction::Malformed(why) => assert!(why.contains("finite"), "{bad}: {why}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
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
            {"update_id": 13, "edited_message": {"chat": {"id": 42}, "text": "/closeall"}}
        ]
    });
    assert_eq!(
        parse_updates(&body),
        vec![
            TelegramUpdate {
                update_id: 11,
                chat_id: 42,
                from_id: 7,
                from_username: Some("alice".into()),
                text: "/status".into()
            },
            TelegramUpdate {
                update_id: 12,
                chat_id: 42,
                from_id: 7,
                from_username: None,
                text: String::new()
            },
            TelegramUpdate {
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
                "text": "/closeall"}},
            // No `from` at all — an anonymous admin or a channel post.
            {"update_id": 2, "message": {"chat": {"id": -100777}, "text": "/closeall"}},
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
    let reason = confirm_reason(4242, 55, Some("bob"), "/closeall hyperliquid");
    assert!(reason.contains("telegram chat 4242"), "the audit line must say WHERE: {reason}");
    // …and WHO — the forensic property the chat id alone cannot give in a group.
    assert!(reason.contains("user 55"), "the audit line must say WHO: {reason}");
    assert!(reason.contains("@bob"), "the handle rides along for legibility: {reason}");
    assert!(reason.ends_with("/closeall hyperliquid"), "verbatim instruction: {reason}");

    // An unattributable sender says so rather than claiming "user 0".
    let anon = confirm_reason(-100777, UNKNOWN_USER_ID, None, "/flatten hyperliquid BTC");
    assert!(anon.contains("user unknown"), "{anon}");
    assert!(!anon.contains("user 0"), "0 must never be rendered as an id: {anon}");
}

/// Preview and confirm are two messages and, in a group, can be two PEOPLE — the token binds to
/// the chat, not the sender. The executed command's audit line therefore names both.
#[test]
fn confirmed_by_appends_the_authorizing_actor_to_the_preview_reason() {
    let previewed = confirm_reason(-100777, 55, Some("bob"), "/closeall");
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

/// A parsed submit with the id the preview would mint (the mint itself is `vike-client`'s, tested
/// there: `vike_client::verbs::fill_order_client_id`).
fn with_order_client_id(cmd: CommandWire, order_client_id: &str) -> CommandWire {
    let CommandWire::Submit(mut r) = cmd else { panic!("a submit") };
    assert!(r.order_client_id.is_empty(), "the grammar leaves the id to the mint");
    r.order_client_id = order_client_id.to_string();
    CommandWire::Submit(r)
}

#[test]
fn describe_shows_everything_that_varies() {
    let Instruction::Write(cmd) = parse_instruction("/submit hyperliquid BTC buy 0.5 64000") else {
        panic!("expected a write");
    };
    let line = describe(&with_order_client_id(*cmd, "tg-1"));
    for needle in ["hyperliquid", "BTC", "buy", "0.5", "64000", "tg-1"] {
        assert!(line.contains(needle), "{needle:?} missing from {line:?}");
    }
}
