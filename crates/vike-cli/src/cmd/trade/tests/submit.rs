//! Why a write is refused, the REPL's order grammar, and the unsizeable orders all three refuse.

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

// -- the REPL, the one-shot verb and the MCP tool refuse the SAME unsizeable orders ----------------

/// One input class, spelled as the OPERATOR TYPES it — a token, so `nan` and `inf` are expressible —
/// and the reason ALL THREE paths must give for it, each behind its own prefix.
struct Unsizeable {
    class: &'static str,
    qty: &'static str,
    /// The text after the `@`, or `None` for a market order.
    price: Option<&'static str>,
    reason: &'static str,
}

/// ⚠ `str::parse::<f64>` ACCEPTS `nan`, `inf` and `-inf` (any case), so a parse that succeeded
/// proves nothing about the number — which is exactly how `+inf` and `@nan` used to reach the
/// preview. The `qty NaN` and `qty zero` rows passed before this table existed (the old check was
/// `is_nan() || <= 0`); `qty +inf`, `qty -inf` and the three price rows are the ones that did not.
const UNSIZEABLE: &[Unsizeable] = &[
    Unsizeable { class: "qty zero", qty: "0", price: None, reason: "qty must be > 0" },
    Unsizeable { class: "qty negative", qty: "-1", price: None, reason: "qty must be > 0" },
    Unsizeable {
        class: "qty NaN",
        qty: "nan",
        price: None,
        reason: "qty must be a finite number, got NaN",
    },
    Unsizeable {
        class: "qty +inf",
        qty: "inf",
        price: None,
        reason: "qty must be a finite number, got inf",
    },
    Unsizeable {
        class: "qty -inf",
        qty: "-inf",
        price: None,
        reason: "qty must be a finite number, got -inf",
    },
    Unsizeable {
        class: "price NaN",
        qty: "1",
        price: Some("nan"),
        reason: "price must be a finite number, got NaN",
    },
    Unsizeable {
        class: "price +inf",
        qty: "1",
        price: Some("inf"),
        reason: "price must be a finite number, got inf",
    },
    Unsizeable {
        class: "price -inf",
        qty: "1",
        price: Some("-inf"),
        reason: "price must be a finite number, got -inf",
    },
];

/// **The agreement, over every class in [`UNSIZEABLE`].** Each path is driven through its OWN real
/// parser — the REPL's `parse_line`, the one-shot verb's `parse_submit_args`, the MCP tool's
/// `verb_from_tool_args` — and each must refuse with the SAME reason in its own prefix, and the
/// one-shot verb on the usage rung. Before the shared function they disagreed: the two command
/// lines refused only `qty <= 0` / NaN, and the tool refused none of it.
#[test]
fn the_repl_the_one_shot_verb_and_the_mcp_tool_refuse_the_same_unsizeable_orders() {
    let finite = |t: &str| t.parse::<f64>().is_ok_and(f64::is_finite);
    let mut mcp_legs = 0;
    for c in UNSIZEABLE {
        let price_tok = c.price.map(|p| format!("@{p}"));

        // 1. the REPL line.
        let mut line = format!("submit binance BTCUSDT buy {}", c.qty);
        if let Some(tok) = &price_tok {
            line.push(' ');
            line.push_str(tok);
        }
        assert_eq!(
            parse_line(&line).unwrap_err(),
            format!("submit: {}", c.reason),
            "REPL, {}: {line}",
            c.class
        );

        // 2. the one-shot verb's argv (book first), on the USAGE rung.
        let mut argv = vec!["binance", "BTCUSDT", "buy", c.qty];
        if let Some(tok) = &price_tok {
            argv.push(tok.as_str());
        }
        let e = super::order::parse_submit_args(&argv).unwrap_err();
        assert_eq!(e.exit, crate::exit::Exit::Usage, "one-shot, {}: {argv:?}", c.class);
        assert_eq!(e.msg, format!("submit: {}", c.reason), "one-shot, {}: {argv:?}", c.class);

        // 3. the MCP tool. JSON has no spelling for NaN or an infinity (`json!` of one is `null`,
        // refused as a MISSING field — pinned in `verbs_tests`), so a non-finite row has no leg
        // here and runs on the first two alone; the counter below proves the finite rows did run.
        if finite(c.qty) && c.price.is_none_or(finite) {
            let mut args = json!({
                "venue": "binance", "symbol": "BTCUSDT", "side": 1,
                "qty": c.qty.parse::<f64>().unwrap(),
            });
            if let Some(p) = c.price {
                args["order_type"] = json!("limit");
                args["price"] = json!(p.parse::<f64>().unwrap());
            }
            assert_eq!(
                verbs::verb_from_tool_args("submit_order", &args).unwrap_err(),
                format!("submit_order: {}", c.reason),
                "MCP, {}: {args}",
                c.class
            );
            mcp_legs += 1;
        }
    }
    assert!(mcp_legs >= 2, "the MCP leg must actually run for the finite rows, ran {mcp_legs}");
}

/// **The other direction, and the one a refusal is judged by.** A sizeable order — over EVERY cap
/// included — still parses on the REPL and is still only ADVISED about: the preview line says OVER
/// LIMIT and the order is still built. The refusal is for an order with no size, never for a big one.
#[test]
fn a_sizeable_order_over_every_cap_still_parses_and_is_only_advised() {
    let caps = verbs::GuardrailCaps { max_qty: Some(1.0), max_notional: Some(10.0) };
    for line in [
        "submit binance BTCUSDT buy 5 @100", // over the qty cap AND the notional cap
        "buy binance BTCUSDT 5",             // a market order: over the qty cap, no price to size
        "sell binance BTCUSDT 5 @market --reduce-only",
        "submit binance BTCUSDT buy 1e3 @1e6", // an enormous but FINITE order
    ] {
        let verb = parse_line(line).unwrap_or_else(|e| panic!("{line:?} must still parse: {e}"));
        let cmd = verb.to_wire_command().expect("a submit is a write");
        let advisory = verbs::guardrail_check(&cmd, caps).line();
        assert!(advisory.contains("OVER LIMIT"), "{line:?} is advised, not refused: {advisory}");
    }
}

/// **A ZERO or NEGATIVE price is NOT refused here, and that is a decision rather than an omission.**
/// The core's risk gate checks only the SIZE for positivity, and a negative price is meaningful on
/// a venue's combo net, so refusing it at the keyboard could refuse an order the node accepts. This
/// pins today's behaviour so a change to it is a deliberate one.
#[test]
fn a_zero_or_negative_price_is_left_to_the_node() {
    for (line, price) in
        [("submit binance BTCUSDT buy 1 @0", 0.0), ("submit binance BTCUSDT buy 1 @-5", -5.0)]
    {
        let o = submit(parse_line(line).unwrap_or_else(|e| panic!("{line:?}: {e}")));
        assert_eq!(o.order_type, "limit", "{line}");
        assert_eq!(o.price, Some(price), "{line}");
    }
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
