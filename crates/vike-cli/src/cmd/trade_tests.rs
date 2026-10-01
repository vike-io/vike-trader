use super::*;

// -- why writes are disabled: three causes, three messages -------------------------------------

/// **The reported defect.** With `tradehub_control` off at the node and a VALID control key in
/// the credential store, a write printed
/// `writes disabled (OBSERVE-ONLY): no VIKE_TRADEHUB_CONTROL_KEY — nothing was sent`. The key was
/// present and fine; the node had refused the SCOPE, and had said so — once, at connect time,
/// before scrolling away. The operator was sent hunting for a key that was never the problem.
#[test]
fn a_scope_refusal_never_reports_itself_as_a_missing_key() {
    let denied = std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "tradehub control auth denied: control disabled on this node",
    );
    let cause = NoControl::from_connect_error(&denied);
    assert_eq!(
        cause,
        NoControl::Refused("tradehub control auth denied: control disabled on this node".into())
    );

    let line = cause.refusal_line();
    assert!(
        !line.contains(&format!("no {}", nodekeys::CONTROL_KEY_ENV)),
        "the message must not blame the key when the key authenticated: {line}"
    );
    assert!(
        line.contains("control disabled on this node"),
        "it must carry the node's own \
             reason, which is the actionable half: {line}"
    );
    assert!(line.contains("tradehub_control"), "…and name the flag that fixes it: {line}");
    assert!(line.contains("nothing was sent"), "every cause must state this: {line}");
}

/// The case the old message WAS right about keeps saying so — the fix must not make the honest
/// path vaguer to make the dishonest one honest.
#[test]
fn an_absent_key_still_says_the_key_is_absent() {
    let line = NoControl::NoKey.refusal_line();
    assert!(line.contains(nodekeys::CONTROL_KEY_ENV), "{line}");
    assert!(line.contains("node-key store"), "it must name BOTH places we looked: {line}");
    assert!(line.contains("nothing was sent"), "{line}");
}

/// A transport failure is neither of the above: nothing is known about the key or the node's
/// policy, so the message claims neither. Classified by ERROR KIND, not by message text — the
/// node is free to reword its reason, and `PermissionDenied` is the one kind `node_handshake`
/// maps `Response::AuthDenied` to.
#[test]
fn an_unreachable_node_blames_neither_the_key_nor_the_node_policy() {
    let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused");
    let cause = NoControl::from_connect_error(&refused);
    assert!(matches!(cause, NoControl::Unreachable(_)), "{cause:?}");

    let line = cause.refusal_line();
    assert!(!line.contains(&format!("no {}", nodekeys::CONTROL_KEY_ENV)), "{line}");
    assert!(!line.contains("tradehub_control"), "nothing is known about the node's flag: {line}");
    assert!(line.contains("Connection refused") && line.contains("nothing was sent"), "{line}");
}

/// The three are genuinely different ACTIONS — put a key in the store / turn the node's flag on /
/// fix the network — so no two may print the same line.
#[test]
fn the_three_causes_print_three_different_lines() {
    let lines = [
        NoControl::NoKey.refusal_line(),
        NoControl::Refused("control disabled on this node".into()).refusal_line(),
        NoControl::Unreachable("Connection refused".into()).refusal_line(),
    ];
    for (i, a) in lines.iter().enumerate() {
        for b in lines.iter().skip(i + 1) {
            assert_ne!(a, b, "two distinct causes print the same message");
        }
    }
}

fn submit(v: Verb) -> WireOrderRequest {
    match v {
        Verb::Submit(o) => o,
        other => panic!("expected Submit, got {other:?}"),
    }
}

#[test]
fn parse_submit_limit_with_reduce_only() {
    let v = parse_line("submit binance BTCUSDT buy 0.5 @59000 --reduce-only").unwrap();
    assert!(v.is_write());
    let o = submit(v);
    assert_eq!(o.venue, "binance");
    assert_eq!(o.symbol, "BTCUSDT");
    assert_eq!(o.side, 1);
    assert_eq!(o.qty, 0.5);
    assert_eq!(o.order_type, "limit");
    assert_eq!(o.price, Some(59000.0));
    assert!(o.reduce_only);
    // The PARSER leaves it empty; `run_write` mints before the preview (see the two tests
    // below and `verbs::fill_client_order_id`). Keeping the parser pure is what makes every
    // grammar test here deterministic.
    assert!(o.client_order_id.is_empty());
}

/// **THE defect, at this surface.** A parsed `submit` carries no coid, and the node REFUSES a
/// remote submit with an empty one — so what actually goes on the wire must be the MINTED
/// command. This pins the exact composition `run_write` performs (mint, then describe), which
/// is the step that was missing entirely.
#[test]
fn a_submit_reaches_the_wire_with_a_minted_venue_valid_coid() {
    let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
    let WireCommand::Submit(unminted) = &cmd else { panic!("expected Submit") };
    assert!(unminted.client_order_id.is_empty(), "the parser mints nothing");

    let mut coids = verbs::coid_minter();
    let sent = verbs::fill_client_order_id(cmd, &mut coids);
    let WireCommand::Submit(o) = &sent else { panic!("still a Submit") };
    assert!(
        vike_model::is_valid_crypto_coid(&o.client_order_id),
        "the node refuses an empty coid and the venue refuses a non-alphanumeric one: {:?}",
        o.client_order_id
    );
    // …and the operator can SEE it: the preview line carries the handle they type back.
    let preview = describe_command(&sent);
    assert!(
        preview.contains(&format!("coid={}", o.client_order_id)),
        "the preview must show the id that is sent: {preview}"
    );
}

/// `--coid` pins an id (both flag forms), and an unusable one is a clean parse error rather
/// than a silent fallback to a minted id the caller did not ask for.
#[test]
fn coid_override_pins_the_id_and_rejects_an_unusable_one() {
    for line in ["submit sim BTCUSDT buy 1 @100 --coid abc123", "buy sim BTCUSDT 1 --coid=abc123"] {
        let o = submit(parse_line(line).unwrap());
        assert_eq!(o.client_order_id, "abc123", "{line}");
    }
    // A pinned id survives the mint untouched — that is what "pin" means.
    let cmd = parse_line("submit sim BTCUSDT buy 1 --coid abc123").unwrap().to_wire_command();
    let mut coids = verbs::coid_minter();
    let WireCommand::Submit(o) = verbs::fill_client_order_id(cmd.unwrap(), &mut coids) else {
        panic!("still a Submit")
    };
    assert_eq!(o.client_order_id, "abc123");

    assert!(parse_line("submit sim BTCUSDT buy 1 --coid").is_err()); // dangling value
    assert!(parse_line("submit sim BTCUSDT buy 1 --coid=").is_err()); // empty is not a pin
    assert!(parse_line("submit sim BTCUSDT buy 1 --coid has space").is_err()); // eats a positional
    assert!(parse_line("submit sim BTCUSDT buy 1 --coid c-1").is_err()); // '-' is not venue-safe
    assert!(parse_line(&format!("submit sim B buy 1 --coid {}", "x".repeat(33))).is_err());
}

#[test]
fn parse_submit_market_default_and_explicit() {
    // No @price token → market.
    let o = submit(parse_line("submit sim ETHUSDT sell 2").unwrap());
    assert_eq!(o.side, -1);
    assert_eq!(o.order_type, "market");
    assert_eq!(o.price, None);
    assert!(!o.reduce_only);
    // Explicit @market → market.
    let o = submit(parse_line("submit sim ETHUSDT sell 2 @market").unwrap());
    assert_eq!(o.order_type, "market");
    assert_eq!(o.price, None);
}

#[test]
fn parse_submit_buy_sell_sugar() {
    let o = submit(parse_line("buy binance BTCUSDT 1.0 @100").unwrap());
    assert_eq!(o.side, 1);
    assert_eq!(o.price, Some(100.0));
    let o = submit(parse_line("sell binance BTCUSDT 1.0").unwrap());
    assert_eq!(o.side, -1);
    assert_eq!(o.order_type, "market");
}

#[test]
fn parse_submit_rejects_bad_input() {
    assert!(parse_line("submit binance BTCUSDT buy").is_err()); // missing qty
    assert!(parse_line("submit binance BTCUSDT sideways 1").is_err()); // bad side
    assert!(parse_line("submit binance BTCUSDT buy notaqty").is_err()); // bad qty
    assert!(parse_line("submit binance BTCUSDT buy 0").is_err()); // qty must be > 0
    assert!(parse_line("submit binance BTCUSDT buy -1").is_err()); // negative qty
    assert!(parse_line("submit binance BTCUSDT buy 1 @notaprice").is_err()); // bad price
    assert!(parse_line("submit only three tokens").is_err()); // too few positionals
}

#[test]
fn parse_cancel() {
    let v = parse_line("cancel c-123").unwrap();
    assert!(v.is_write());
    assert_eq!(v, Verb::Cancel("c-123".into()));
    assert!(parse_line("cancel").is_err());
    assert!(parse_line("cancel a b").is_err());
}

#[test]
fn parse_modify_variants() {
    assert_eq!(
        parse_line("modify c-1 --qty 2.0").unwrap(),
        Verb::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None }
    );
    assert_eq!(
        parse_line("modify c-1 --price 100 --qty 3").unwrap(),
        Verb::Modify { client_order_id: "c-1".into(), new_qty: Some(3.0), new_price: Some(100.0) }
    );
    assert!(parse_line("modify c-1").is_err()); // nothing to change
    assert!(parse_line("modify").is_err()); // no coid
    assert!(parse_line("modify c-1 --qty").is_err()); // dangling value
    assert!(parse_line("modify c-1 --bogus 1").is_err()); // unknown flag
}

#[test]
fn parse_flatten() {
    assert_eq!(
        parse_line("flatten binance BTCUSDT").unwrap(),
        Verb::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into(), account: None }
    );
    assert!(parse_line("flatten binance").is_err());
    assert!(parse_line("flatten a b c").is_err());
}

#[test]
fn parse_market_exit() {
    assert_eq!(parse_line("market-exit").unwrap(), Verb::MarketExit { venue: None, account: None });
    assert_eq!(
        parse_line("market-exit binance").unwrap(),
        Verb::MarketExit { venue: Some("binance".into()), account: None }
    );
    assert_eq!(parse_line("panic").unwrap(), Verb::MarketExit { venue: None, account: None });
    assert!(parse_line("market-exit a b").is_err());
}

#[test]
fn parse_mass_cancel() {
    assert_eq!(
        parse_line("mass-cancel").unwrap(),
        Verb::MassCancel { venue: None, symbol: None, account: None }
    );
    assert_eq!(
        parse_line("mass-cancel binance").unwrap(),
        Verb::MassCancel { venue: Some("binance".into()), symbol: None, account: None }
    );
    assert_eq!(
        parse_line("mass-cancel binance BTCUSDT").unwrap(),
        Verb::MassCancel {
            venue: Some("binance".into()),
            symbol: Some("BTCUSDT".into()),
            account: None
        }
    );
    assert!(parse_line("mass-cancel a b c").is_err());
}

/// **The read and the two writes are three separate WORDS** (ruling 17), and each carries the
/// `is_write` classification the preview+confirm gate routes on. `status` is the read; `halt`
/// and `resume` each resolve to the one state they name.
#[test]
fn parse_status_reads_and_halt_resume_write() {
    let v = parse_line("status").unwrap();
    assert!(!v.is_write(), "status is a READ");
    assert_eq!(v, Verb::Status);
    for (line, want) in [("halt", WireTradingState::Halted), ("resume", WireTradingState::Active)] {
        let v = parse_line(line).unwrap();
        assert!(v.is_write(), "{line} CHANGES the node's mode");
        assert_eq!(v, Verb::SetState(want));
    }
}

/// **The defect ruling 17 removed, pinned as a property rather than as a spelling.** `state` is
/// gone from the grammar with no alias and no deprecation shim, and — the half that matters —
/// NO read verb can be turned into a mode WRITE by adding a token: every candidate below is a
/// clean parse error, so there is no line in this REPL that halts a live daemon except the one
/// word that says `halt`.
#[test]
fn no_word_plus_an_argument_can_change_the_trading_mode() {
    for line in [
        "state",
        "state halted",
        "state active",
        "state reducing",
        "status halted",
        "status active",
        "halt now",
        "resume all",
    ] {
        let parsed = parse_line(line);
        assert!(parsed.is_err(), "{line:?} must not parse, got {parsed:?}");
    }
    // …and nothing that DOES parse as a read is a write.
    for line in ["status", "orders", "positions", "equity", "snapshot", "recent"] {
        assert!(!parse_line(line).unwrap().is_write(), "{line} must stay a read");
    }
}

/// **The removed word gets typed at 3am, so the refusal names its replacements.**
///
/// `state` is gone and stays gone — this is an `Err`, not an alias, because a shim that still
/// worked would keep alive the exact `state halted` line ruling 17 deleted. But an operator
/// pasting a stale runbook line into a live node's prompt must not be handed a bare
/// "unknown command" and sent off to find a page: the sentence has to carry the READ and both
/// WRITES. Pinned for the bare word AND for the argument form, since `state halted` is the
/// spelling a runbook actually holds.
#[test]
fn the_removed_state_verb_answers_with_the_words_that_replaced_it() {
    for line in ["state", "state halted", "state active"] {
        let err = parse_line(line).unwrap_err();
        assert!(err.contains("REMOVED"), "{line:?}: {err}");
        assert!(err.contains("status"), "{line:?} must name the READ: {err}");
        assert!(err.contains("halt"), "{line:?} must name the WRITE: {err}");
        assert!(err.contains("resume"), "{line:?} must name the way back: {err}");
    }
    // …and it is a REFUSAL: nothing parses, so nothing can be sent.
    assert!(parse_line("state halted").is_err());
}

/// `halt` and `resume` build exactly the wire command the node's kill switch reads, and the
/// preview line names the state — so the operator confirming a halt sees `Halted`, not a verb.
#[test]
fn halt_and_resume_reach_the_wire_as_set_trading_state() {
    assert_eq!(
        parse_line("halt").unwrap().to_wire_command().unwrap(),
        WireCommand::SetTradingState(WireTradingState::Halted)
    );
    assert_eq!(
        parse_line("resume").unwrap().to_wire_command().unwrap(),
        WireCommand::SetTradingState(WireTradingState::Active)
    );
    assert!(
        describe_command(&WireCommand::SetTradingState(WireTradingState::Halted))
            .contains("Halted")
    );
}

/// The one-shot verbs' argv grammar, and the two properties that are safety rather than
/// convenience: `--yes` defaults OFF (so an unattended run stops at the prompt unless it says
/// otherwise) and `--reason` is carried, so a halt in a runbook can record WHY.
#[test]
fn the_one_shot_mode_grammar_takes_node_yes_and_reason() {
    let parse = |argv: &[&str]| parse_mode_args(argv.iter().map(|s| s.to_string()));
    let a = parse(&["--node", "127.0.0.1:9200"]).unwrap();
    assert_eq!(a.node.as_deref(), Some("127.0.0.1:9200"));
    assert!(!a.yes, "--yes is OFF unless asked for");
    assert_eq!(a.reason, None);

    let a = parse(&["--node=n:1", "-y", "--reason", "feed gap on binance"]).unwrap();
    assert_eq!(a.node.as_deref(), Some("n:1"));
    assert!(a.yes);
    assert_eq!(a.reason.as_deref(), Some("feed gap on binance"));

    assert_eq!(parse(&["--yes=1"]).unwrap_err(), "--yes takes no value");
    assert!(parse(&["--wat"]).unwrap_err().contains("--wat"));
    assert_eq!(parse(&["-h"]).unwrap_err(), args::HELP_SENTINEL);
    // A bare mode word is not an argument of these verbs either — `halt reducing` is a usage
    // error, not a third state.
    assert!(parse(&["reducing"]).unwrap_err().contains("reducing"));
}

/// The verb NAME is derived from the state it sends, so a message can never say `halt` while
/// sending `Active`, and each usage block states what its own verb does.
#[test]
fn the_one_shot_verb_name_follows_the_state_it_sends() {
    assert_eq!(mode_verb(WireTradingState::Halted), "halt");
    assert_eq!(mode_verb(WireTradingState::Active), "resume");
    let halt = mode_usage("halt");
    assert!(halt.contains("vike-cli trade halt --node"), "{halt}");
    // The covered-reduce exemption is stated where an operator reads it under pressure: a halt
    // that reads as "I am trapped" gets un-halted, restarting the strategy that caused it.
    assert!(halt.contains("market-exit") && halt.contains("flatten"), "{halt}");
    assert!(mode_usage("resume").contains("ACTIVE"), "{}", mode_usage("resume"));
}

/// Only an explicit `y`/`yes` confirms — an empty line, a stray word and whitespace are all NO.
/// One predicate, both readers (the REPL's line editor and the one-shot's plain stdin), so the
/// two surfaces cannot come to disagree about what a confirmation is.
#[test]
fn only_an_explicit_yes_confirms() {
    for yes in ["y", "Y", "yes", "  YES  \n"] {
        assert!(is_yes(yes), "{yes:?}");
    }
    for no in ["", "\n", "n", "no", "halt", "ye", "1"] {
        assert!(!is_yes(no), "{no:?}");
    }
}

#[test]
fn parse_read_verbs_are_not_writes() {
    for line in [
        "orders",
        "orders BTCUSDT",
        "positions",
        "positions binance",
        "equity",
        "balance",
        "snapshot",
        "snap",
        "recent",
        "recent 10",
    ] {
        let v = parse_line(line).unwrap();
        assert!(!v.is_write(), "{line:?} must be a read");
        assert!(v.to_wire_command().is_none(), "{line:?} maps to no wire command");
    }
    assert_eq!(parse_line("orders BTCUSDT").unwrap(), Verb::Orders(Some("BTCUSDT".into())));
    assert_eq!(parse_line("positions binance").unwrap(), Verb::Positions(Some("binance".into())));
    assert_eq!(parse_line("recent 10").unwrap(), Verb::Recent(Some(10)));
    assert_eq!(parse_line("recent").unwrap(), Verb::Recent(None));
    assert!(parse_line("recent notanum").is_err());
    assert!(parse_line("orders a b").is_err());
}

#[test]
fn parse_meta_and_unknown() {
    assert_eq!(parse_line("help").unwrap(), Verb::Help);
    assert_eq!(parse_line("quit").unwrap(), Verb::Quit);
    assert_eq!(parse_line("exit").unwrap(), Verb::Quit);
    assert!(parse_line("").is_err());
    assert!(parse_line("   ").is_err());
    assert!(parse_line("frobnicate").is_err());
}

/// Each WRITE verb maps to the correct [`WireCommand`]; each READ verb maps to `None`.
#[test]
fn verb_to_wire_command_mapping() {
    // Submit
    let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
    match cmd {
        WireCommand::Submit(o) => {
            assert_eq!(o.symbol, "BTCUSDT");
            assert_eq!(o.side, 1);
            assert_eq!(o.qty, 1.0);
            assert_eq!(o.price, Some(100.0));
        }
        other => panic!("expected Submit, got {other:?}"),
    }
    assert_eq!(
        parse_line("cancel c-9").unwrap().to_wire_command().unwrap(),
        WireCommand::Cancel("c-9".into())
    );
    assert_eq!(
        parse_line("modify c-9 --qty 5").unwrap().to_wire_command().unwrap(),
        WireCommand::Modify { client_order_id: "c-9".into(), new_qty: Some(5.0), new_price: None }
    );
    assert_eq!(
        parse_line("flatten sim BTCUSDT").unwrap().to_wire_command().unwrap(),
        WireCommand::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None }
    );
    assert_eq!(
        parse_line("mass-cancel sim").unwrap().to_wire_command().unwrap(),
        WireCommand::MassCancel { venue: Some("sim".into()), symbol: None, account: None }
    );
    assert_eq!(
        parse_line("market-exit").unwrap().to_wire_command().unwrap(),
        WireCommand::MarketExit { venue: None, account: None }
    );
    assert_eq!(
        parse_line("halt").unwrap().to_wire_command().unwrap(),
        WireCommand::SetTradingState(WireTradingState::Halted)
    );
}

// ---- the `--reason` tail (node proto v4) --------------------------------------------------

#[test]
fn split_reason_takes_the_rest_of_the_line_verbatim() {
    let (cmd, why) = split_reason("flatten binance BTCUSDT --reason CPI print in 2 minutes");
    assert_eq!(cmd, "flatten binance BTCUSDT");
    assert_eq!(why.as_deref(), Some("CPI print in 2 minutes"));
    // …and the command half still parses exactly as it would have without the tail.
    assert_eq!(
        parse_line(cmd).unwrap(),
        Verb::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into(), account: None }
    );
}

#[test]
fn split_reason_absent_returns_the_whole_line_untouched() {
    let line = "submit binance BTCUSDT buy 0.5 @59000 --reduce-only";
    assert_eq!(split_reason(line), (line, None));
}

#[test]
fn split_reason_strips_one_layer_of_quotes_and_treats_blank_as_none() {
    assert_eq!(split_reason("cancel c-1 --reason \"why not\"").1.as_deref(), Some("why not"));
    assert_eq!(split_reason("cancel c-1 --reason 'why not'").1.as_deref(), Some("why not"));
    // Only ONE layer, and only when it matches on both ends.
    assert_eq!(split_reason("cancel c-1 --reason \"a' ").1.as_deref(), Some("\"a'"));
    // Nothing (or only whitespace) after the flag is NO rationale.
    assert_eq!(split_reason("cancel c-1 --reason").1, None);
    assert_eq!(split_reason("cancel c-1 --reason    ").1, None);
    assert_eq!(split_reason("cancel c-1 --reason").0, "cancel c-1");
}

#[test]
fn split_reason_only_matches_a_whole_token() {
    // A coid/symbol that merely CONTAINS the text is not a split point.
    for line in ["cancel my--reason-x", "cancel --reasonable", "cancel x--reason"] {
        assert_eq!(split_reason(line), (line, None), "{line:?} must not split");
    }
    // …but a real flag later in the same line still does.
    let (cmd, why) = split_reason("cancel --reasonable --reason it was a typo");
    assert_eq!(cmd, "cancel --reasonable");
    assert_eq!(why.as_deref(), Some("it was a typo"));
}

#[test]
fn split_reason_leaves_the_verb_grammar_alone() {
    // A rationale containing flag-shaped words never reaches `parse_line` (the whole point of
    // splitting first): `modify` still sees only its own flags.
    let (cmd, why) = split_reason("modify c-1 --qty 2 --reason was --price too aggressive");
    assert_eq!(
        parse_line(cmd).unwrap(),
        Verb::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None }
    );
    assert_eq!(why.as_deref(), Some("was --price too aggressive"));
}

#[test]
fn config_parsing() {
    let c = parse_config(["--node", "the CI box:7979", "--yes"].into_iter().map(String::from)).unwrap();
    assert_eq!(c.node.as_deref(), Some("the CI box:7979"));
    assert!(c.yes);
    assert!(!c.help);
    // `--node=host:port` inline form.
    let c = parse_config(["--node=127.0.0.1:9".to_string()].into_iter()).unwrap();
    assert_eq!(c.node.as_deref(), Some("127.0.0.1:9"));
    // help short-circuits.
    let c = parse_config(["--help".to_string()].into_iter()).unwrap();
    assert!(c.help);
    // unknown arg errors.
    assert!(parse_config(["--bogus".to_string()].into_iter()).is_err());
    // dangling value errors.
    assert!(parse_config(["--node".to_string()].into_iter()).is_err());
    // a bare boolean rejects an inline value (the shared args glue; `--yes=1` used to be
    // silently accepted as true).
    assert!(parse_config(["--yes=1".to_string()].into_iter()).is_err());
}

// -- USAGE names the plane: every group, the group-less trio, and the REPL's own note --------

/// The roster is DERIVED from [`plane::GROUPS`], never re-typed, so a group added there cannot
/// silently go unnamed in the text `--help` prints.
#[test]
fn the_usage_names_every_group_in_the_roster() {
    for (name, _) in plane::GROUPS {
        assert!(USAGE.contains(name), "the usage must name the group '{name}'");
    }
}

/// The trailing note is kept VERBATIM across the plane design — the REPL still has no
/// `--json`, and this string is the one place that decision is stated in words rather than
/// merely implied by an absent flag.
#[test]
fn the_usage_still_states_that_the_repl_has_no_json() {
    assert!(
        USAGE.contains("no --json on the REPL"),
        "the absence is a DECISION a reader can see, not an oversight somebody will fix"
    );
}

/// `status`/`halt`/`resume` are the one exemption from the group layer, and the usage text
/// must still name all three even though none of them appears in [`plane::GROUPS`].
#[test]
fn the_usage_names_the_three_words_that_take_no_group() {
    for word in ["status", "halt", "resume"] {
        assert!(USAGE.contains(word));
    }
}

// -- the orders table ----------------------------------------------------------------------

/// A Polymarket-shaped order: a long decimal token id for a symbol, and a coid at the upper end
/// of what `submit --coid` accepts.
fn order(coid: &str, symbol: &str) -> vike_tradehub_client::wire::WireOrderView {
    vike_tradehub_client::wire::WireOrderView {
        client_order_id: coid.to_string(),
        venue: "polymarket".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 10.0,
        order_type: "Limit".to_string(),
        price: Some(0.51),
        trigger_price: None,
        status: "Accepted".to_string(),
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

fn snap_with(orders: Vec<vike_tradehub_client::wire::WireOrderView>) -> WireSnapshot {
    WireSnapshot { orders, ..WireSnapshot::empty() }
}

/// The coid is the HANDLE — it is what the operator types back at `cancel <coid>` — so it must
/// survive the table whole. The column was a fixed 20 while `submit --coid` accepts 32, so a
/// pinned id came back `aaaaaaaaaaaaaaaaaaa…` and could not be retyped at all.
#[test]
fn the_coid_column_is_never_truncated() {
    let coid = "A".repeat(32);
    let table = orders_table(&snap_with(vec![order(&coid, "BTCUSDT")]), None);
    assert!(table.contains(&coid), "the coid must survive whole:\n{table}");
    assert!(!table.contains('…'), "nothing on this row is long enough to shorten:\n{table}");
}

/// The symbol IS capped (a Polymarket token id is a ~77-digit decimal and would push every later
/// column off the terminal) but truncated in the MIDDLE: the tokens of one up/down family share
/// a long prefix and differ at the TAIL, so a head-only cell named every order in the family
/// identically — the reported `DUMMYTOKEN000…`.
#[test]
fn a_long_symbol_keeps_both_ends() {
    let a = format!("{}0001", "7".repeat(72));
    let b = format!("{}9999", "7".repeat(72));
    let table = orders_table(&snap_with(vec![order("c-1", &a), order("c-2", &b)]), None);

    assert!(table.contains("0001") && table.contains("9999"), "tails must survive:\n{table}");
    assert!(table.contains("7777"), "…and so must the head:\n{table}");
    // The two rows must be TELLABLE APART, which is the whole defect.
    let lines: Vec<&str> = table.lines().filter(|l| l.contains('…')).collect();
    assert_eq!(lines.len(), 2, "both rows truncate:\n{table}");
    assert_ne!(lines[0], lines[1], "two token ids rendered identically:\n{table}");
}

/// An ordinary instrument is untouched — the cap is generous enough that widening it cost the
/// common case nothing.
#[test]
fn ordinary_symbols_are_not_truncated() {
    for symbol in ["BTCUSDT", "EUR/USD", "BTC-30AUG26-120000-C"] {
        let table = orders_table(&snap_with(vec![order("c-1", symbol)]), None);
        assert!(table.contains(symbol), "{symbol} was shortened:\n{table}");
    }
}

#[test]
fn trunc_mid_keeps_both_ends_and_degrades_cleanly() {
    assert_eq!(trunc_mid("abcdef", 6), "abcdef", "a fitting string is untouched");
    assert_eq!(trunc_mid("abcdefghij", 5), "ab…ij");
    assert_eq!(trunc_mid("abcdefghij", 4), "ab…j", "an odd budget favours the head");
    assert_eq!(trunc_mid("abcdefghij", 2), "a…");
    assert_eq!(trunc_mid("abcdefghij", 1), "…");
    assert_eq!(trunc_mid("abcdefghij", 0), "");
    // Char-counted, not byte-counted — a multi-byte symbol must not panic on a slice boundary.
    assert_eq!(trunc_mid("ααααββββ", 5), "αα…ββ");
}

#[test]
fn width_of_fits_the_header_and_honours_the_cap() {
    assert_eq!(width_of("coid", ["ab"].into_iter(), None), 4, "never narrower than the header");
    assert_eq!(width_of("coid", ["abcdefgh"].into_iter(), None), 8, "uncapped grows to fit");
    assert_eq!(width_of("coid", ["abcdefgh"].into_iter(), Some(5)), 5, "capped stops");
    assert_eq!(width_of("symbol", std::iter::empty(), Some(24)), 6, "no rows ⇒ the header");
}

/// The empty view still renders (and still carries its `[seq N]` stamp).
#[test]
fn an_empty_orders_table_says_none() {
    let table = orders_table(&snap_with(Vec::new()), None);
    assert!(table.contains("(none)"), "{table}");
    assert!(table.contains("[seq 0]"), "{table}");
}

// ---- the strategy-lifecycle grammar (mount / unmount / set-setting) ----------------------

/// The one entry the REPL loop calls, resolved through the SHARED construction site to the
/// wire command it produces — which is what the assertions below are about, and what
/// [`run_write`] is handed.
fn lifecycle(line: &str) -> WireCommand {
    let verb = parse_repl_line(line)
        .unwrap_or_else(|e| panic!("expected a lifecycle verb for {line:?}, got error: {e}"));
    assert!(verb.is_write(), "{line:?} must parse to a WRITE verb, got {verb:?}");
    verb.to_wire_command()
        .unwrap_or_else(|| panic!("{line:?}: a write verb must build a wire command"))
}

/// The router keeps ONE grammar per line: the lifecycle verbs are claimed by
/// [`parse_lifecycle`] and land on their own [`Verb`] variants, everything else still falls
/// through to the order vocabulary untouched, and an unknown verb is still the ORDER grammar's
/// own error (the lifecycle half must not swallow it).
///
/// ⚠ This used to assert on a `Parsed::Lifecycle` / `Parsed::Order` union. With both grammars
/// producing one [`Verb`], the routing claim is made on the VARIANT instead — the same
/// property, asserted one level down, and still the thing that would break if the lifecycle
/// half started claiming (or stopped claiming) a token.
#[test]
fn parse_repl_line_routes_each_verb_to_exactly_one_grammar() {
    for line in [
        "mount sim BTCUSDT 1m --name spread_maker",
        "unmount grid-a",
        "set-setting flags.reconcile_off true",
        "set flags.reconcile_off true",
    ] {
        assert!(
            matches!(
                parse_repl_line(line),
                Ok(Verb::MountStrategy { .. }
                    | Verb::UnmountStrategy { .. }
                    | Verb::SetSetting { .. })
            ),
            "{line:?} must be claimed by the lifecycle grammar"
        );
    }
    for line in ["submit sim BTCUSDT buy 1", "orders", "halt", "quit"] {
        assert!(
            !matches!(
                parse_repl_line(line),
                Ok(Verb::MountStrategy { .. }
                    | Verb::UnmountStrategy { .. }
                    | Verb::SetSetting { .. })
            ),
            "{line:?} must fall through to the order vocabulary"
        );
        // …and PARSE there. Without this the negative above would also be satisfied by an
        // error, which is the one way "fell through" could be true and useless.
        assert!(parse_repl_line(line).is_ok(), "{line:?} must still parse");
    }
    // The order grammar still owns the unknown-verb error, and `quit` still round-trips as the
    // value the REPL loop breaks on.
    assert!(parse_repl_line("frobnicate").is_err());
    assert!(parse_repl_line("   ").is_err());
    assert_eq!(parse_repl_line("quit").unwrap(), Verb::Quit);
}

/// The minimal registry-name mount: no explicit id (the node derives one), no params (an empty
/// table).
#[test]
fn mount_by_registry_name_defaults_the_id_and_the_params() {
    assert_eq!(
        lifecycle("mount binance BTCUSDT 1m --name spread_maker"),
        WireCommand::MountStrategy {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: None,
            name: Some("spread_maker".into()),
            rhai: None,
            params: json!({}),
        }
    );
}

/// The Rhai form, with every optional field supplied — and `--params` carrying the SPACES a
/// person actually types, which is the whole reason it runs to end of line rather than being
/// one whitespace-delimited token.
#[test]
fn mount_by_rhai_path_takes_an_id_and_a_spacey_params_object() {
    assert_eq!(
        lifecycle(
            "mount sim ETHUSDT 5m --rhai /srv/vike-<unit>/strategies/grid.rhai --id grid-a \
                 --params {\"gamma\": 0.0008, \"levels\": 4}"
        ),
        WireCommand::MountStrategy {
            venue: "sim".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            interval: "5m".into(),
            controller_id: Some("grid-a".into()),
            name: None,
            rhai: Some("/srv/vike-<unit>/strategies/grid.rhai".into()),
            params: json!({"gamma": 0.0008, "levels": 4}),
        }
    );
    // The `--flag=value` spelling is the same pair `submit --coid` accepts.
    assert_eq!(
        lifecycle("mount sim ETHUSDT 5m --name=spread_maker --id=grid-b"),
        WireCommand::MountStrategy {
            venue: "sim".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            interval: "5m".into(),
            controller_id: Some("grid-b".into()),
            name: Some("spread_maker".into()),
            rhai: None,
            params: json!({}),
        }
    );
}

/// ⚠ **`--account` has THREE states and the REPL must keep them apart**, because at two engines
/// of one venue they are three different answers: omitted names no account (the node REFUSES
/// it there), `DEFAULT` names the venue's unlabelled account deliberately, and a label names
/// that account. The tests above already pin the omitted state on every mount they parse; this
/// one pins the other two, in both flag spellings.
#[test]
fn mount_carries_the_account_in_both_flag_spellings() {
    let WireCommand::MountStrategy { account, .. } =
        lifecycle("mount binance BTCUSDT 1m --name spread_maker --account ALT")
    else {
        panic!("a mount")
    };
    assert_eq!(account.as_deref(), Some("ALT"));

    // ⚠ `DEFAULT` must survive as the STRING, not be normalised away into `None`: absence and
    // `DEFAULT` are different rows on the wire, and collapsing them here would silently turn a
    // deliberate choice into the refusable one.
    let WireCommand::MountStrategy { account, .. } =
        lifecycle("mount binance BTCUSDT 1m --name=spread_maker --account=DEFAULT")
    else {
        panic!("a mount")
    };
    assert_eq!(account.as_deref(), Some("DEFAULT"));
}

/// An illegal label is refused HERE, naming the flag and the offending value — not a round trip
/// away as a `Response::Error`. The grammar is `parse_wire_account`'s, so this edge adds a
/// message rather than a second set of rules.
#[test]
fn mount_refuses_an_illegal_account_label_at_parse_time() {
    let err = parse_repl_line("mount binance BTCUSDT 1m --name spread_maker --account alt")
        .expect_err("a lowercase label is not a legal account");
    assert!(err.contains("--account"), "names the flag: {err}");
    assert!(err.contains("alt"), "…and the value it refused: {err}");
    assert!(err.contains("DEFAULT"), "…and the spelling that names the default: {err}");
}

/// **The XOR, refused at PARSE time and naming BOTH spellings.** `WireCommand::MountStrategy`'s
/// contract is an exclusive choice; a client that let the node discover the violation spends a
/// round trip to say what the grammar already knew, and answers with the node's words rather
/// than with the two flags the operator has to choose between.
#[test]
fn mount_refuses_both_or_neither_strategy_source_and_names_the_two_spellings() {
    for line in
        ["mount sim BTCUSDT 1m --name spread_maker --rhai /tmp/s.rhai", "mount sim BTCUSDT 1m"]
    {
        let err = parse_repl_line(line).expect_err(line);
        assert!(err.contains("--name"), "{line:?} must name --name: {err}");
        assert!(err.contains("--rhai"), "{line:?} must name --rhai: {err}");
    }
}

/// `--params` is the `[strategy.params]` TABLE, so a non-object (or unparseable) payload is a
/// clean refusal rather than something the node has to decode and reject. An EMPTY `--params`
/// is refused too and is not the same as omitting the flag: mounting a strategy on defaults
/// nobody chose is not what "I typed --params" meant.
#[test]
fn mount_params_must_be_a_json_object_and_an_empty_flag_is_not_an_empty_table() {
    let base = "mount sim BTCUSDT 1m --name spread_maker";
    assert!(parse_repl_line(&format!("{base} --params [1,2]")).is_err(), "an array");
    assert!(parse_repl_line(&format!("{base} --params 7")).is_err(), "a scalar");
    assert!(parse_repl_line(&format!("{base} --params {{oops")).is_err(), "not JSON");
    let err = parse_repl_line(&format!("{base} --params")).expect_err("empty --params");
    assert!(err.contains("--params"), "{err}");
    // …while OMITTING it is the empty table, and always was.
    let WireCommand::MountStrategy { params, .. } = lifecycle(base) else { panic!("a mount") };
    assert_eq!(params, json!({}));
}

#[test]
fn mount_rejects_malformed_argument_shapes() {
    assert!(parse_repl_line("mount sim BTCUSDT --name spread_maker").is_err(), "no interval");
    assert!(parse_repl_line("mount a b c d --name s").is_err(), "a fourth positional");
    assert!(parse_repl_line("mount a b c --name").is_err(), "dangling value");
    // A flag eaten as another flag's value is the `submit --coid has space` failure again.
    assert!(parse_repl_line("mount a b c --id --name s").is_err(), "a flag as a value");
    assert!(parse_repl_line("mount a b c --name s --name t").is_err(), "twice");
    assert!(parse_repl_line("mount a b c --name s --bogus 1").is_err(), "unknown flag");
}

/// `unmount` takes ONE token. A second is refused rather than ignored, and the message points
/// at where a mount id comes from — this client deliberately does not re-derive the node's
/// `{venue}__{symbol}__{interval}` rule.
#[test]
fn unmount_takes_one_mount_id() {
    assert_eq!(
        lifecycle("unmount grid-a"),
        WireCommand::UnmountStrategy { controller_id: "grid-a".into() }
    );
    assert_eq!(
        lifecycle("unmount binance__BTCUSDT__1m"),
        WireCommand::UnmountStrategy { controller_id: "binance__BTCUSDT__1m".into() }
    );
    assert!(parse_repl_line("unmount").is_err());
    assert!(parse_repl_line("unmount binance BTCUSDT 1m").is_err());
}

/// **`set-setting <full.dotted.key> <value>` — the KEY names its section, and the wire's `file` is
/// derived from it.** A settings write is one row named by its key (`docs/decisions/0086`), the
/// same two positionals `vike-cli config set` takes. The value runs to END OF LINE and is taken
/// VERBATIM — quotes included, because they are what tells the node's TOML parse that `"250"` is a
/// string and not the integer `250`.
#[test]
fn set_setting_takes_the_key_and_the_value_to_end_of_line_without_unquoting_it() {
    assert_eq!(
        lifecycle("set-setting policy.max_notional_per_order 250"),
        WireCommand::SetSetting {
            // Derived from the key's first segment: the released v0.1.35 daemon still requires the
            // field on decode, and the key is the only thing that may decide it.
            file: "policy".into(),
            key: "policy.max_notional_per_order".into(),
            value: "250".into(),
            confirm: None,
        }
    );
    // Spaces survive: a TOML array is a value a person types with spaces in it.
    let WireCommand::SetSetting { file, value, .. } =
        lifecycle("set config.venues [\"binance\", \"okx\"]")
    else {
        panic!("a settings write")
    };
    assert_eq!(file, "config");
    assert_eq!(value, "[\"binance\", \"okx\"]");
    // …and the quotes are NOT stripped, unlike `--reason`'s one forgiving layer.
    let WireCommand::SetSetting { value, .. } =
        lifecycle("set-setting config.tradehub_addr \"127.0.0.1:7979\"")
    else {
        panic!("a settings write")
    };
    assert_eq!(value, "\"127.0.0.1:7979\"");

    assert!(parse_repl_line("set-setting policy.x").is_err(), "no value");
    assert!(parse_repl_line("set-setting").is_err());
}

/// **The retired `<file>` token is refused BY NAME, never silently re-read.** The grammar was
/// `set-setting <file> <key> <value>` until the key alone named the row; a line still spelled that
/// way would otherwise parse its file NAME as the key and the real key as the start of the value —
/// a write of `policy.toml = "policy.max_notional_per_order 250"` for the node to puzzle over. The
/// refusal names the spelling that replaced it, the same courtesy the removed `state` verb gets.
#[test]
fn the_retired_file_token_is_refused_by_name() {
    for line in [
        "set-setting policy.toml policy.max_notional_per_order 250",
        "set-setting policy policy.max_notional_per_order 250",
        "set config.toml config.tradehub_addr 127.0.0.1:7979",
    ] {
        let err = parse_repl_line(line).expect_err(line);
        assert!(err.contains("no file"), "{line}: say what was retired: {err}");
        assert!(
            err.contains("set-setting <full.dotted.key> <value"),
            "{line}: …and name the spelling that replaced it: {err}"
        );
    }
}

/// **Every REPL settings write goes out with `confirm: None`, for every section.** The node has
/// ignored the wire's field since `docs/decisions/0086` point 7 — no retype confirm, for any key —
/// and nothing between this grammar and the wire fills one in. Asserted on the WIRE command the
/// line becomes, because that is what reaches the node; `crate::cmd::verbs`'
/// `every_settings_write_is_built_with_no_confirm` holds the shared builder to the same rule for
/// the `mcp` surface.
///
/// ⚠ This replaced two tests that pinned the ceremony: that the parser could never PRE-FILL the
/// confirm (the retype prompt was what filled it), and which keys the prompt was asked for
/// (`vike_config::is_policy_plane_key`, deleted on 2026-09-29 with its last caller).
#[test]
fn a_repl_settings_write_carries_no_confirm() {
    for line in [
        "set-setting policy.max_notional_per_order 250",
        "set-setting flags.reconcile_off true",
        "set preferences.theme dark",
    ] {
        let WireCommand::SetSetting { confirm, .. } = lifecycle(line) else {
            panic!("a settings write: {line}")
        };
        assert_eq!(confirm, None, "nothing may ride in the wire's confirm field: {line}");
    }
}

/// `--reason` still runs to end of line, and it composes with `--params`, which does too: the
/// rationale is split off FIRST, so the mount parser never sees it and the JSON keeps its
/// spaces.
#[test]
fn a_reason_tail_composes_with_the_rest_of_line_params() {
    let (line, why) = split_reason(
        "mount sim BTCUSDT 1m --name spread_maker --params {\"gamma\": 0.0008} --reason \
             trialling a tighter quote",
    );
    assert_eq!(why.as_deref(), Some("trialling a tighter quote"));
    let WireCommand::MountStrategy { params, name, .. } = lifecycle(line) else {
        panic!("a mount")
    };
    assert_eq!(params, json!({"gamma": 0.0008}));
    assert_eq!(name.as_deref(), Some("spread_maker"));
}

/// The shared rest-of-line splitter distinguishes ABSENT from PRESENT-AND-EMPTY, which is the
/// one property its two callers answer differently: an empty `--reason` is no rationale, an
/// empty `--params` is a typo.
#[test]
fn split_tail_separates_an_absent_flag_from_an_empty_one() {
    assert_eq!(split_tail("mount a b c", PARAMS_FLAG), ("mount a b c", None));
    assert_eq!(split_tail("mount a b c --params", PARAMS_FLAG), ("mount a b c", Some("")));
    assert_eq!(
        split_tail("mount a b c --params   {\"x\": 1}", PARAMS_FLAG),
        ("mount a b c", Some("{\"x\": 1}"))
    );
    // Whole-token only — a symbol that merely CONTAINS the text is not a split point.
    assert_eq!(split_tail("mount a --paramsy c", PARAMS_FLAG), ("mount a --paramsy c", None));
}

/// The preview is the last thing between an operator and a running strategy, so it must show
/// the mount's IDENTITY, its SOURCE and its knobs — and, for a settings write, the assignment and
/// nothing about a confirm.
#[test]
fn the_lifecycle_previews_carry_what_the_operator_has_to_check() {
    let preview = describe_command(&lifecycle(
        "mount binance BTCUSDT 1m --name spread_maker --id grid-a --params {\"gamma\": 0.5}",
    ));
    for needle in ["binance", "BTCUSDT", "1m", "grid-a", "spread_maker", "gamma"] {
        assert!(preview.contains(needle), "the preview must carry {needle}: {preview}");
    }
    // A mount with no explicit id shows the ABSENCE rather than inventing the node's derived
    // one, and a Rhai mount shows the script path as its source.
    let preview = describe_command(&lifecycle("mount sim ETHUSDT 5m --rhai /srv/s.rhai"));
    assert!(preview.contains("id=-"), "no id was given: {preview}");
    assert!(preview.contains("/srv/s.rhai"), "the source is the script path: {preview}");

    let preview = describe_command(&lifecycle("unmount grid-a"));
    assert!(preview.contains("UNMOUNT-STRATEGY") && preview.contains("grid-a"), "{preview}");

    // A settings write previews the ASSIGNMENT. The `old → new` beside it needs the node's current
    // value, so it is printed by `run_write` from a node read, not rendered here.
    let preview = describe_command(&lifecycle("set-setting policy.max_notional_per_order 250"));
    assert!(
        preview.contains("policy.max_notional_per_order") && preview.contains("250"),
        "{preview}"
    );
    // ⚠ …and never a confirm marker, not even for a command an OLDER client minted with the wire's
    // `confirm` field filled: the node ignores that field (`docs/decisions/0086` point 7), so a
    // `[confirmed]` badge would advertise a guard that does not exist.
    let from_an_older_client = WireCommand::SetSetting {
        file: "policy.toml".into(),
        key: "policy.max_notional_per_order".into(),
        value: "250".into(),
        confirm: Some("policy.max_notional_per_order".into()),
    };
    let preview = describe_command(&from_an_older_client);
    assert!(!preview.contains("confirm"), "no confirm marker of any spelling: {preview}");
}
