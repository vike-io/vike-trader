use super::*;
use crate::cmd::trade::help_of;
use crate::exit::Exit;
use vike_tradehub_client::wire::CommandWire;

/// `trade order <args…>` read through the plane's one parser; a `Stop` is a refusal (or help).
fn parsed(args: &[&str]) -> Result<OrderVerb, args::Stop> {
    let argv = std::iter::once("order").chain(args.iter().copied()).map(String::from);
    match crate::cmd::trade::parse(argv)?.sub {
        Some(crate::cmd::trade::Sub::Order { verb }) => Ok(verb),
        other => panic!("`trade order` did not read as the order group: {other:?}"),
    }
}

/// `trade order ls <args…>`, through the parser and `ls`'s own rules.
fn ls(args: &[&str]) -> Result<selector::LsArgs, String> {
    let line: Vec<&str> = std::iter::once("ls").chain(args.iter().copied()).collect();
    match parsed(&line).map_err(|stop| stop.to_string())? {
        OrderVerb::Ls(cmd) => cmd.into_args(),
        other => unreachable!("`ls` read as {other:?}"),
    }
}

// The bare group, help, an unknown verb and `ls --help` are the grammar the three verb groups
// share, tested once over all three: `crates/vike-cli/src/cmd/trade/tests/reason_and_help.rs`'s
// `VERB_GROUPS`.

#[test]
fn ls_requires_a_node() {
    let e = ls(&[]).unwrap_err();
    assert!(e.contains("--node"), "{e}");
}

#[test]
fn ls_parses_a_leading_book_selector_and_the_flags() {
    let a = ls(&["binance", "--symbol", "BTCUSDT", "--node", "n:1", "--json"])
        .expect("a valid ls line parses");
    assert_eq!(a.node, "n:1");
    assert_eq!(a.symbol.as_deref(), Some("BTCUSDT"));
    assert!(a.json);
    assert_eq!(a.book.expect("a book was given").venue, "binance");
}

#[test]
fn ls_with_no_positional_names_no_book() {
    let a = ls(&["--node", "n:1"]).expect("a bare ls parses");
    assert!(a.book.is_none());
}

// ---- the WRITE verbs' own parsers (task 7) -------------------------------------------------

#[test]
fn submit_parses_the_book_first_then_the_instrument() {
    // A BARE venue, deliberately — this test is about positional ORDER (book, then symbol,
    // side, qty, order type), not about the account label, and a labelled book now refuses
    // (see `submit_refuses_a_labelled_book_the_node_cannot_route`).
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    match v {
        Verb::Submit(o) => {
            assert_eq!(o.venue, "binance");
            assert_eq!(o.symbol, "BTCUSDT");
            assert_eq!(o.side, 1);
            assert_eq!(o.qty, 0.01);
            assert_eq!(o.order_type, "market");
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn a_price_token_makes_it_a_limit() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@61000"]).unwrap();
    match v {
        Verb::Submit(o) => {
            assert_eq!(o.order_type, "limit");
            assert_eq!(o.price, Some(61_000.0));
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

/// The parser leaves the order client id empty, as the REPL's does; `oneshot::execute_write` mints it with the CLI's
/// letter BEFORE the preview, off the node's frame when one was read, so the id shown is the id sent.
#[test]
fn the_order_client_id_is_minted_before_the_preview_so_the_id_shown_is_the_id_sent() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    let cmd = v.to_wire_command().expect("a write");
    assert!(matches!(&cmd, CommandWire::Submit(o) if o.order_client_id.is_empty()), "{cmd:?}");
    let mut minter =
        verbs::order_client_id_minter(vike_model::orders::client_id::OrderClientIdSource::Cli);
    match verbs::fill_order_client_id(cmd, &mut minter, None, |_| true) {
        CommandWire::Submit(o) => assert!(
            o.order_client_id.starts_with('C')
                && vike_model::is_valid_crypto_order_client_id(&o.order_client_id),
            "a node REFUSES a remote Submit with an empty order_client_id: {:?}",
            o.order_client_id
        ),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn a_missing_book_is_a_usage_error_naming_the_forms() {
    let e = parse_submit_args(&["BTCUSDT", "buy", "0.01"]).expect_err("the book is required");
    assert!(e.message.contains(vike_model::accounts::account_keys::BOOK_FORMS));
}

/// ⚠ **This test has now flipped THREE times, and this flip follows the node rather than a
/// belief about it.** Task 7 replaced an earlier refusal test with one of THIS name, on the
/// strength of the wire field and the advertised capability — both true, and both one layer
/// short of the node, whose `lower_command` built a `vike_model::OrderRequest` with no
/// `account` field. The account-routing critical fix then flipped it back to
/// `submit_refuses_a_labelled_book_the_node_cannot_route`, pinning a local refusal
/// (`crate::cmd::trade::selector`'s `refuse_an_unroutable_account`) that named that missing field
/// as the cause.
///
/// Stage 5 of `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md`
/// DELETED that refusal, because the node half it waited on is on `main` and this branch now
/// carries it: `OrderRequest` has an `account`, `crates/vike-tradehub/src/server/control.rs`'s
/// `lower_command` reads it back, routing resolves it to the engine it names, and an account the
/// node does not hold is refused at the node's edge before the Ack. So the parser's job is to
/// PASS the label through unaltered — which is what this pins — and the refusal of an unheld
/// account is pinned over a real node, where it now lives, by
/// `crates/vike-cli/tests/shipped_cli/trade_plane_cli.rs`'s
/// `a_labelled_submit_for_an_unheld_account_is_refused_by_the_node_not_the_cli`.
#[test]
fn a_labelled_book_threads_its_account_onto_the_wire() {
    let v = parse_submit_args(&["binance/ALT", "BTCUSDT", "buy", "0.01", "@market"])
        .expect("a labelled book is no longer refused locally");
    match v {
        Verb::Submit(o) => {
            assert_eq!(o.venue, "binance");
            assert_eq!(
                o.account.as_deref(),
                Some("ALT"),
                "the label must reach the wire exactly as named — the node routes on it"
            );
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

/// **A bare venue and `<venue>/DEFAULT` are different words on a write** (decision 0254): the wire's three
/// states ([`OrderRequestWire::account`]) carry the difference. The bare one names no account, which the
/// node's door takes only where the venue has one; `/DEFAULT` names the unlabelled account.
#[test]
fn a_bare_venue_names_no_account_and_default_names_the_unlabelled_one() {
    let account = |book: &str| match parse_submit_args(&[book, "BTCUSDT", "buy", "0.01", "@market"])
    {
        Ok(Verb::Submit(o)) => o.account,
        other => panic!("expected a Submit, got {other:?}"),
    };
    assert_eq!(account("binance"), None);
    assert_eq!(account("binance/DEFAULT").as_deref(), Some("DEFAULT"));
    assert_eq!(account("binance/ALT").as_deref(), Some("ALT"));
}

#[test]
fn submit_reduce_only_and_pinned_order_client_id_survive_the_book_first_grammar() {
    let v = parse_submit_args(&[
        "okx",
        "BTC-USDT",
        "sell",
        "2",
        "@100",
        "--reduce-only",
        "--order-client-id",
        "MYID001",
    ])
    .unwrap();
    match v {
        Verb::Submit(o) => {
            assert!(o.reduce_only);
            assert_eq!(o.order_client_id, "MYID001");
            assert_eq!(o.side, -1);
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

/// The one-shot verb's half of "a refusal is for an order with NO size, never for a big one": a
/// sizeable submit over EVERY advisory cap still parses, and the preview's guardrail still says OVER
/// LIMIT about it. (The refusal side — the same classes the REPL and the MCP tool refuse, on the
/// usage rung — is `crate::cmd::trade`'s
/// `the_repl_and_the_one_shot_verb_refuse_the_same_unsizeable_orders`, which drives
/// three parsers off one table.)
#[test]
fn a_sizeable_over_limit_submit_still_parses_and_is_only_advised() {
    let capabilities =
        verbs::GuardrailCapabilities { max_qty: Some(1.0), max_notional: Some(10.0) };
    for argv in [
        vec!["binance", "BTCUSDT", "buy", "5", "@100"], // over the qty cap AND the notional cap
        vec!["binance", "BTCUSDT", "buy", "5"],         // a market order: over the qty cap only
        vec!["binance/ALT", "BTCUSDT", "sell", "1e3", "@1e6", "--reduce-only"], // huge but FINITE
    ] {
        let verb = parse_submit_args(&argv)
            .unwrap_or_else(|e| panic!("{argv:?} must still parse: {}", e.message));
        let cmd = verb.to_wire_command().expect("a submit is a write");
        let advisory =
            verbs::guardrail_check(&cmd, capabilities, Ok(Default::default()), false).line();
        assert!(advisory.contains("OVER LIMIT"), "{argv:?} is advised, not refused: {advisory}");
    }
}

#[test]
fn cancel_takes_exactly_one_order_client_id_and_no_book() {
    let v = parse_cancel_args(&["c-1"]).unwrap();
    assert_eq!(v, Verb::Cancel("c-1".to_string()));
    assert!(parse_cancel_args(&[]).is_err());
    assert!(parse_cancel_args(&["c-1", "extra"]).is_err());
}

#[test]
fn modify_requires_at_least_one_change() {
    let e = parse_modify_args(&["c-1"]).expect_err("nothing to change is an error");
    assert!(e.message.contains("nothing to change"));
    let v = parse_modify_args(&["c-1", "--qty", "2"]).unwrap();
    assert_eq!(
        v,
        Verb::Modify {
            order_client_id: "c-1".to_string(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        }
    );
}

/// `modify C --qty 5 --price 1e999` parsed as inf, previewed `price=inf`, and the JSON frame sent `null`, so
/// the node changed only the qty. A number that is not finite is a usage error; a zero is the node's to judge.
#[test]
fn a_modify_number_that_is_not_finite_is_refused() {
    for (flag, raw, what) in [
        ("--price", "1e999", "price"),
        ("--price", "inf", "price"),
        ("--price", "nan", "price"),
        ("--qty", "1e999", "qty"),
        ("--qty", "inf", "qty"),
        ("--qty", "nan", "qty"),
    ] {
        let e = parse_modify_args(&["C", flag, raw]).expect_err(raw);
        assert_eq!(e.exit, Exit::Usage, "{flag} {raw}");
        assert_eq!(e.message, format!("modify: {what} must be a finite number, got {raw:?}"));
    }
    assert_eq!(
        parse_modify_args(&["C", "--qty", "0", "--price", "1e300"]).unwrap(),
        Verb::Modify {
            order_client_id: "C".into(),
            new_qty: Some(0.0),
            new_price: Some(1e300),
            new_trigger_price: None,
        }
    );
}

/// `modify C --qty 5 --reason why --price 100` changed only the qty: `--reason` takes the rest of the
/// line. The swallowed flag is refused and named; before `--reason`, and as a word that is no flag of
/// this verb, nothing changes.
#[test]
fn a_modify_flag_after_reason_is_refused_not_swallowed() {
    for (argv, flag) in [
        (vec!["C", "--qty", "5", "--reason", "why", "--price", "100"], "--price"),
        (vec!["C", "--price", "100", "--reason", "why", "--qty=5"], "--qty"),
    ] {
        let e = parse_modify_args(&argv).expect_err("a swallowed flag is refused");
        assert_eq!(e.exit, Exit::Usage, "{argv:?}");
        assert!(
            e.message.contains(&format!("Put `{flag}` BEFORE --reason")),
            "{argv:?}: {}",
            e.message
        );
    }
    let v = parse_modify_args(&["C", "--qty", "5", "--price", "100", "--reason", "why", "--tp"]);
    assert_eq!(
        v.unwrap(),
        Verb::Modify {
            order_client_id: "C".into(),
            new_qty: Some(5.0),
            new_price: Some(100.0),
            new_trigger_price: None,
        }
    );
}

#[test]
fn mass_cancel_book_is_optional_and_a_bare_venue_names_no_account() {
    let v = parse_mass_cancel_args(&[]).unwrap();
    assert_eq!(v, Verb::MassCancel { venue: None, symbol: None, account: None });

    let v = parse_mass_cancel_args(&["binance", "BTCUSDT"]).unwrap();
    assert_eq!(
        v,
        Verb::MassCancel {
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            account: None
        }
    );
    let v = parse_mass_cancel_args(&["binance/DEFAULT"]).unwrap();
    assert_eq!(
        v,
        Verb::MassCancel {
            venue: Some("binance".to_string()),
            symbol: None,
            account: Some("DEFAULT".to_string())
        }
    );
}

/// A labelled book on `mass-cancel` is no longer refused HERE — this replaces
/// `mass_cancel_refuses_a_labelled_book_the_node_cannot_route`, which pinned the local refusal
/// stage 5 deleted (see this module's doc). The parser now passes the label through, and what
/// keeps it off a node that would drop it is the CLIENT's capability gate:
/// `crates/vike-tradehub-client/src/tradehub_control.rs`'s `required_feature` answers
/// `FEATURE_ACCOUNT_SCOPED_REDUCE` for a `MassCancel` naming an account, which no node
/// advertises until it can confine the cancel to that account. That refusal is pinned over a
/// real node by `crates/vike-cli/tests/shipped_cli/trade_plane_cli.rs`'s
/// `a_labelled_risk_reducing_verb_is_refused_client_side_before_anything_is_sent`. Naming NO
/// book at all (the first case above) is untouched — no account, so no gate sees one.
#[test]
fn mass_cancel_threads_a_labelled_book_onto_the_wire() {
    let v = parse_mass_cancel_args(&["binance/ALT"])
        .expect("a labelled book is no longer refused locally");
    assert_eq!(
        v,
        Verb::MassCancel {
            venue: Some("binance".to_string()),
            symbol: None,
            account: Some("ALT".to_string())
        }
    );
}

/// `--help` must not promise what the client gate refuses. `mass-cancel`'s help and its roster row
/// in the group's help offered a book as an ordinary narrowing ("optionally scoped to one book"),
/// while ANY named account on a risk-reducing verb — the bare venue's positive `DEFAULT`
/// included — needs a node advertising `account-scoped-reduce` and is refused client-side,
/// before anything is sent, against one that does not. Pinned so the omission cannot grow back.
#[test]
fn mass_cancel_help_names_the_capability_a_book_needs() {
    let text = help_of(&["order", "mass-cancel"]);
    assert!(text.contains("account-scoped-reduce"), "{text}");
    assert!(text.contains("DEFAULT"), "a bare venue names its DEFAULT account\n{text}");
    let group = help_of(&["order"]);
    assert!(group.contains("account-scoped-reduce"), "the roster row names it too: {group}");
}

/// The wrapper flags are read ANYWHERE among the tokens, in both spellings, and `--reason`
/// swallows the rest of the argv (a flag included) as its rationale — the contract the old
/// hand-written wrapper walk had, now `clap`'s.
#[test]
fn the_wrapper_flags_sit_anywhere_and_reason_swallows_the_rest() {
    let OrderVerb::Submit(c) = parsed(&[
        "submit", "binance", "--node", "n:1", "BTCUSDT", "buy", "--yes", "1", "@100", "--reason",
        "scale", "in", "--json",
    ])
    .expect("a submit with flags between its positionals reads") else {
        panic!("expected a submit");
    };
    assert_eq!(c.tokens, ["binance", "BTCUSDT", "buy", "1", "@100"]);
    assert_eq!(c.wrap.node.as_deref(), Some("n:1"));
    assert!(c.wrap.yes);
    assert!(!c.wrap.json, "a --json after --reason is part of the rationale");
    assert_eq!(c.wrap.reason().as_deref(), Some("scale in --json"));
}

/// A repeated `--node` is last-wins, `-y`/`--yes` may repeat, and `--order-client-id=ID`, `--node=ADDR` and a
/// `--reason` beginning with a single dash are all read — each an argv an operator already types.
#[test]
fn repeats_and_inline_spellings_are_read() {
    let OrderVerb::Submit(c) = parsed(&[
        "submit",
        "binance",
        "BTCUSDT",
        "buy",
        "1",
        "--node",
        "a:1",
        "--node=b:2",
        "-y",
        "--yes",
        "--order-client-id=abc123",
        "--reason",
        "-urgent",
    ])
    .expect("the spellings read") else {
        panic!("expected a submit");
    };
    assert_eq!(c.wrap.node.as_deref(), Some("b:2"), "the last --node wins");
    assert!(c.wrap.yes);
    assert_eq!(c.order_client_id.as_deref(), Some("abc123"));
    assert_eq!(c.wrap.reason().as_deref(), Some("-urgent"));
}

/// An unknown flag is refused by name (it used to fall through as a positional and fail the count),
/// and the `@price` token may sit anywhere among the positionals.
#[test]
fn an_unknown_flag_is_refused_and_the_price_token_floats() {
    let e = parse_submit_args(&["binance", "BTCUSDT", "buy", "1", "--bogus"]).unwrap_err();
    assert!(e.message.contains("--bogus"), "{}", e.message);
    let v = parse_submit_args(&["binance", "BTCUSDT", "@100", "buy", "1"]).unwrap();
    match v {
        Verb::Submit(o) => assert_eq!(o.price, Some(100.0)),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

// ---- the TP/SL bracket: `bracket` -----------------------------------------------------------------

fn bracket_of(argv: &[&str]) -> OrderBracketWire {
    match parse_bracket_args(argv).unwrap_or_else(|e| panic!("{argv:?}: {}", e.message)) {
        Verb::Bracket(b) => b,
        other => panic!("expected a Bracket, got {other:?}"),
    }
}

/// The IG case the proof is for: a market entry, both exits as flags. The book is REQUIRED and reads
/// exactly as `submit`'s does (a bare venue names no account): the wire's three states are argued at
/// `OrderRequestWire::account`.
#[test]
fn a_bracket_takes_the_book_first_and_both_exits_as_flags() {
    let b = bracket_of(&[
        "ig",
        "CS.D.EURUSD.CFD.IP",
        "buy",
        "1",
        "--stop-loss",
        "1.05",
        "--take-profit",
        "1.15",
    ]);
    assert_eq!((b.venue.as_str(), b.symbol.as_str()), ("ig", "CS.D.EURUSD.CFD.IP"));
    assert_eq!((b.side, b.qty, b.entry_price), (1, 1.0, None), "no @price is a market entry");
    assert_eq!((b.stop_loss, b.take_profit), (1.05, 1.15));
    assert_eq!(b.account, None);
}

/// `@price` is the entry, wherever it sits among the positionals; `@market` is the market spelling;
/// a sell is side -1; a labelled book reaches the wire unaltered.
#[test]
fn the_entry_is_the_at_token_and_a_label_and_a_side_are_threaded() {
    let limit = bracket_of(&[
        "dukascopy",
        "@150.2",
        "USDJPY",
        "buy",
        "1000",
        "--stop-loss",
        "148",
        "--take-profit",
        "153",
    ]);
    assert_eq!(limit.entry_price, Some(150.2));
    let market = bracket_of(&[
        "dukascopy",
        "USDJPY",
        "sell",
        "1000",
        "@market",
        "--stop-loss",
        "153",
        "--take-profit",
        "148",
    ]);
    assert_eq!((market.side, market.entry_price), (-1, None));
    let labelled = bracket_of(&[
        "binance/ALT",
        "BTCUSDT",
        "buy",
        "1",
        "--stop-loss",
        "1",
        "--take-profit",
        "3",
    ]);
    assert_eq!(labelled.account.as_deref(), Some("ALT"));
}

/// A bracket is an entry WITH both exits: either one missing is a usage error that names the flag,
/// and so is a flag written after `--reason` (it is rationale text there, so the exit is missing).
#[test]
fn a_bracket_without_both_exits_is_a_usage_error_naming_the_flag() {
    for (class, argv, needle) in [
        ("no stop-loss", vec!["ig", "S", "buy", "1", "--take-profit", "9"], "--stop-loss"),
        ("no take-profit", vec!["ig", "S", "buy", "1", "--stop-loss", "1"], "--take-profit"),
        (
            "an exit swallowed by --reason",
            vec![
                "ig",
                "S",
                "buy",
                "1",
                "--stop-loss",
                "1",
                "--reason",
                "why",
                "--take-profit",
                "9",
            ],
            "--take-profit",
        ),
    ] {
        let e = parse_bracket_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(
            e.message.contains(needle) && e.message.contains("required"),
            "{class}: {}",
            e.message
        );
    }
}

/// Every number is finite and above zero, refused before the node is asked. The entry is the one the
/// wire cannot guard: `OrderBracketWire::entry_price` writes a non-finite price as `null`, and the
/// node reads `null` as a MARKET entry.
#[test]
fn a_bracket_number_that_is_not_a_price_is_a_usage_error() {
    let with = |extra: &[&'static str]| {
        let mut argv = vec!["ig", "S", "buy", "1", "--stop-loss", "1", "--take-profit", "9"];
        argv.extend_from_slice(extra);
        argv
    };
    for (class, argv, needle) in [
        ("an infinite entry", with(&["@inf"]), "entry price must be a finite number"),
        ("a NaN entry", with(&["@nan"]), "entry price must be a finite number"),
        ("a zero entry", with(&["@0"]), "entry price must be a finite number"),
        ("a word for an entry", with(&["@soon"]), "not a number for entry price"),
        ("two entries", with(&["@1", "@2"]), "more than one @price token"),
        (
            "a zero qty",
            vec!["ig", "S", "buy", "0", "--stop-loss", "1", "--take-profit", "9"],
            "qty must be a finite number above zero",
        ),
        (
            "an infinite qty",
            vec!["ig", "S", "buy", "inf", "--stop-loss", "1", "--take-profit", "9"],
            "qty must be a finite number above zero",
        ),
        (
            "a NaN stop-loss",
            vec!["ig", "S", "buy", "1", "--stop-loss", "nan", "--take-profit", "9"],
            "--stop-loss must be a finite number",
        ),
        (
            "a zero take-profit",
            vec!["ig", "S", "buy", "1", "--stop-loss", "1", "--take-profit", "0"],
            "--take-profit must be a finite number",
        ),
        (
            "a word for a stop-loss",
            vec!["ig", "S", "buy", "1", "--stop-loss", "low", "--take-profit", "9"],
            "not a number for --stop-loss",
        ),
        (
            "a side that is not one",
            vec!["ig", "S", "hold", "1", "--stop-loss", "1", "--take-profit", "9"],
            "side must be buy|sell",
        ),
    ] {
        let e = parse_bracket_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// Four positionals, book first: three (the book-less count) is the likeliest authoring mistake and
/// names the forms; and the flags of `submit` that mean nothing to a bracket are refused by `clap`
/// rather than ignored (a bracket has no order client id, no `--reduce-only`, no trigger kind).
#[test]
fn a_bracket_refuses_a_missing_book_and_the_flags_it_has_no_use_for() {
    let e = parse_bracket_args(&["S", "buy", "1", "--stop-loss", "1", "--take-profit", "9"])
        .expect_err("the book is required");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.message.contains(vike_model::accounts::account_keys::BOOK_FORMS), "{}", e.message);
    for flag in [["--order-client-id", "abc123"], ["--type", "stop"], ["--trigger", "5"]] {
        let argv =
            ["ig", "S", "buy", "1", "--stop-loss", "1", "--take-profit", "9", flag[0], flag[1]];
        let e = parse_bracket_args(&argv).expect_err(flag[0]);
        assert_eq!(e.exit, Exit::Usage, "{}", flag[0]);
        assert!(e.message.contains(flag[0]), "{}: {}", flag[0], e.message);
    }
    let e =
        parse_bracket_args(&["ig", "S", "buy", "1", "--reduce-only"]).expect_err("--reduce-only");
    assert!(e.message.contains("--reduce-only"), "{}", e.message);
}

/// The verb is a WRITE that resolves to `CommandWire::Bracket` UNCHANGED (no order client id is minted: the
/// wire type has none), and the line the operator answers `y/N` to says what is being sent: the
/// entry, both exits and the account.
#[test]
fn a_bracket_is_a_write_and_its_preview_names_every_leg_and_the_account() {
    let verb = parse_bracket_args(&[
        "ig",
        "EURUSD",
        "buy",
        "1",
        "--stop-loss",
        "1.05",
        "--take-profit",
        "1.15",
    ])
    .unwrap();
    assert!(verb.is_write());
    let cmd = verb.to_wire_command().expect("a bracket is a write");
    let CommandWire::Bracket(spec) = &cmd else { panic!("expected a Bracket, got {cmd:?}") };
    assert_eq!(Verb::Bracket(spec.clone()), verb);
    assert_eq!(
        crate::cmd::trade::describe_command(&cmd),
        "BRACKET buy ig EURUSD qty=1 entry=market sl=1.05 tp=1.15 account=-"
    );
}

/// **A bracket's group stem is in the line the operator confirms** when the mint gives it one (a node offering
/// `bracket-stem`); for an older node the mint gives none and the line is the one above.
#[test]
fn a_minted_bracket_stem_is_in_the_preview() {
    let argv = ["ig", "EURUSD", "buy", "1", "--stop-loss", "1.05", "--take-profit", "1.15"];
    let cmd = parse_bracket_args(&argv).unwrap().to_wire_command().expect("a write");
    let mint = |takes: bool| {
        let mut minter =
            verbs::order_client_id_minter(vike_model::orders::client_id::OrderClientIdSource::Cli);
        verbs::fill_order_client_id(cmd.clone(), &mut minter, None, |_| takes)
    };
    let stemmed = mint(true);
    let CommandWire::Bracket(spec) = &stemmed else {
        panic!("expected a Bracket, got {stemmed:?}")
    };
    let stem = spec.order_client_id.as_deref().expect("a stem where the node takes one");
    assert!(stem.starts_with("C000000"), "{stem}");
    assert_eq!(
        crate::cmd::trade::describe_command(&stemmed),
        format!("BRACKET buy ig EURUSD qty=1 entry=market sl=1.05 tp=1.15 account=- group={stem}")
    );
    assert_eq!(mint(false), cmd, "an older node gets the bracket with no stem");
}

/// `--help` says what the verb needs and what a refusal looks like: both exits, the capability the
/// account owes, the whole-bracket refusal, the no-retry-id warning, and that `--reason` is last.
#[test]
fn the_bracket_help_states_its_exits_its_refusal_and_its_retry_rule() {
    let text = help_of(&["order", "bracket"]);
    for needle in [
        "--stop-loss",
        "--take-profit",
        "bracket-account",
        "WHOLE",
        "SECOND bracket",
        "--reason must be LAST",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    assert!(help_of(&["order"]).contains("bracket"), "the group roster names the verb");
}

// ---- native trigger orders: `--type` + `--trigger` ----------------------------------------------

fn submit_of(argv: &[&str]) -> Verb {
    parse_submit_args(argv).unwrap_or_else(|e| panic!("{argv:?}: {}", e.message))
}

fn request_of(argv: &[&str]) -> OrderRequestWire {
    match submit_of(argv) {
        Verb::Submit(o) => o,
        other => panic!("expected a Submit, got {other:?}"),
    }
}

/// **The lifetime flags.** `--post-only` and `--tif` become the wire word (`post-only`, `ioc`, ...), the default
/// `gtc` (spelled or not) stays the ABSENT field so the frame is the one from before, and a `gtd` order carries its
/// expiry (a UTC instant or epoch milliseconds) beside the word.
#[test]
fn the_lifetime_flags_become_the_wire_word() {
    let po = request_of(&["okx", "BTC-USDT-SWAP", "buy", "1", "@100", "--post-only"]);
    assert_eq!((po.time_in_force.as_deref(), po.gtd_expiry), (Some("post-only"), None));
    for word in ["day", "ioc", "fok", "IOC"] {
        let o = request_of(&["binance", "BTCUSDT", "buy", "1", "@100", "--tif", word]);
        assert_eq!(o.time_in_force.as_deref(), Some(word.to_ascii_lowercase().as_str()), "{word}");
    }
    for argv in [
        vec!["binance", "BTCUSDT", "buy", "1", "@100"],
        vec!["binance", "BTCUSDT", "buy", "1", "@100", "--tif", "gtc"],
    ] {
        let o = request_of(&argv);
        assert_eq!((o.time_in_force, o.gtd_expiry), (None, None), "{argv:?}: the old frame");
    }
    let by_instant = request_of(&[
        "binance",
        "BTCUSDT.P",
        "buy",
        "1",
        "@100",
        "--tif=gtd",
        "--expires",
        "2099-11-01T12:00:00Z",
    ]);
    assert_eq!(by_instant.time_in_force.as_deref(), Some("gtd"));
    assert_eq!(by_instant.gtd_expiry, vike_model::parse_utc_timestamp_ms("2099-11-01T12:00:00Z"));
    let by_ms = request_of(&[
        "binance",
        "BTCUSDT.P",
        "buy",
        "1",
        "@100",
        "--tif",
        "gtd",
        "--expires",
        "4102444800000",
    ]);
    assert_eq!(by_ms.gtd_expiry, Some(4_102_444_800_000));
}

/// Every misuse is a usage error naming the rule, and sends NOTHING: the pair that cannot combine, an
/// expiry that belongs to `gtd` alone (or is missing from it, or past, or seconds), a word that is no
/// lifetime, and a lifetime on an order that is not a limit order.
#[test]
fn a_lifetime_misuse_is_a_usage_error() {
    let base = ["binance", "BTCUSDT", "buy", "1", "@100"];
    let cases: [(&[&str], &str); 10] = [
        (&["--post-only", "--tif", "ioc"], "does not combine with --tif"),
        (&["--tif", "post-only"], "write --post-only"),
        (&["--tif", "gtd"], "needs --expires"),
        (&["--tif", "day", "--expires", "2099-01-01T00:00:00Z"], "belongs to --tif gtd"),
        (&["--tif", "gtd", "--expires", "2001-01-01T00:00:00Z"], "not in the future"),
        (&["--tif", "gtd", "--expires", "1793412600"], "looks like epoch SECONDS"),
        (&["--tif", "gtd", "--expires", "tomorrow"], "not a UTC instant"),
        (&["--tif", "forever"], "expected day, gtc, ioc, fok, gtd, at-open or at-close"),
        (&["--tif", "ioc", "--tif", "fok"], "cannot be used multiple times"),
        (&["--post-only", "--expires", "2099-01-01T00:00:00Z"], "belongs to --tif gtd"),
    ];
    for (extra, want) in cases {
        let argv: Vec<&str> = base.iter().chain(extra.iter()).copied().collect();
        let e = parse_submit_args(&argv).expect_err(&format!("{extra:?}"));
        assert_eq!(e.exit, Exit::Usage, "{extra:?}");
        assert!(e.message.contains(want), "{extra:?}: wanted /{want}/ in: {}", e.message);
    }
    // a lifetime needs a price: a market order has nothing to expire or protect, and a stop's lifetime
    // is its trigger's business
    for extra in [
        vec!["--post-only"],
        vec!["--tif=ioc"],
        vec!["--type", "stop", "--trigger", "90", "--tif", "ioc"],
    ] {
        let argv: Vec<&str> =
            ["binance", "BTCUSDT", "sell", "1"].into_iter().chain(extra.iter().copied()).collect();
        let e = parse_submit_args(&argv).expect_err("a lifetime off the limit path");
        assert!(e.message.contains("belongs to a limit order"), "{argv:?}: {}", e.message);
        assert!(e.message.contains("@<price>"), "it says where the price goes: {}", e.message);
    }
}

/// The preview line the operator answers `y/N` to names the lifetime, and the order that names none reads as
/// it always did.
#[test]
fn the_preview_names_the_lifetime() {
    let line =
        |argv: &[&str]| crate::cmd::trade::describe_command(&CommandWire::Submit(request_of(argv)));
    let plain = line(&["binance", "BTCUSDT", "buy", "1", "@100", "--order-client-id", "c1"]);
    assert!(!plain.contains('['), "{plain}");
    assert!(
        line(&["binance", "BTCUSDT", "buy", "1", "@100", "--post-only"]).contains(" [post-only] ")
    );
    assert!(line(&["binance", "BTCUSDT", "buy", "1", "@100", "--tif", "ioc"]).contains(" [ioc] "));
    let gtd = line(&[
        "binance",
        "BTCUSDT.P",
        "buy",
        "1",
        "@100",
        "--tif",
        "gtd",
        "--expires",
        "2099-11-01T12:00:00Z",
    ]);
    assert!(gtd.contains(" [gtd until 2099-11-01T12:00:00Z] "), "{gtd}");
}

/// `--tif at-open|at-close` is an auction on a market OR a limit order; on a stop it is refused, with no hint to
/// add a price (a market order takes it too).
#[test]
fn an_auction_lifetime_rides_a_market_or_a_limit_order() {
    for price in [None, Some("@100")] {
        for word in ["at-open", "at-close"] {
            let mut argv = vec!["binance", "BTCUSDT", "buy", "1", "--tif", word];
            argv.extend(price);
            assert_eq!(request_of(&argv).time_in_force.as_deref(), Some(word), "{argv:?}");
        }
    }
    let e = parse_submit_args(&[
        "binance",
        "BTCUSDT",
        "sell",
        "1",
        "--type",
        "stop",
        "--trigger",
        "90",
        "--tif",
        "at-open",
    ])
    .expect_err("a stop has no auction");
    assert!(e.message.contains("belongs to a market or a limit order"), "{}", e.message);
    assert!(!e.message.contains("@<price>"), "no price hint for an auction: {}", e.message);
}

/// `--display` rides a limit order onto the wire, and the display rule refuses it off a limit order and at
/// the order's qty or above, as usage errors. A display of 0 is a HIDDEN order since the attributes wave: it
/// rides the wire too, and the venue's `hidden` word judges it at the core edge.
#[test]
fn a_display_rides_a_limit_order_and_its_misuse_is_a_usage_error() {
    let shown = request_of(&["binance", "BTCUSDT", "buy", "5", "@100", "--display", "1"]);
    assert_eq!(shown.display_quantity, Some(1.0));
    assert_eq!(request_of(&["binance", "BTCUSDT", "buy", "5", "@100"]).display_quantity, None);
    let hidden = request_of(&["binance", "BTCUSDT", "buy", "5", "@100", "--display", "0"]);
    assert_eq!(hidden.display_quantity, Some(0.0));
    for (argv, want) in [
        (vec!["binance", "BTCUSDT", "buy", "5", "--display", "1"], "belongs to a limit order"),
        (vec!["binance", "BTCUSDT", "buy", "5", "@100", "--display", "5"], "hides nothing"),
        (
            vec!["binance", "BTCUSDT", "buy", "5", "@100", "--display", "x"],
            "not a display quantity",
        ),
    ] {
        let e = parse_submit_args(&argv).expect_err(&format!("{argv:?}"));
        assert_eq!(e.exit, Exit::Usage, "{argv:?}");
        assert!(e.message.contains(want), "{argv:?}: wanted /{want}/ in: {}", e.message);
    }
}

/// `--at` holds the order until a moment in the future, read as `--expires` reads an instant; a past one, epoch
/// seconds, and one at or after a `gtd` expiry are usage errors. The preview line says when it goes.
#[test]
fn a_release_rides_the_wire_and_its_misuse_is_a_usage_error() {
    let held = request_of(&["binance", "BTCUSDT", "buy", "1", "--at", "2099-11-01T12:00:00Z"]);
    assert_eq!(
        held.release,
        Some(OrderReleaseWire { at: 4_097_217_600_000, price: None, symbol: None, venue: None })
    );
    let line = crate::cmd::trade::describe_command(&CommandWire::Submit(held));
    assert!(line.contains(" [held until 2099-11-01T12:00:00Z] "), "{line}");
    assert_eq!(request_of(&["binance", "BTCUSDT", "buy", "1"]).release, None);
    let conditional = request_of(&[
        "binance",
        "BTCUSDT",
        "buy",
        "1",
        "--at",
        "2099-11-01T12:00:00Z",
        "--when-price",
        "above:100",
    ]);
    assert_eq!(
        conditional.release,
        Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(vike_tradehub_client::wire::OrderReleasePriceWire::Above(100.0)),
            symbol: None,
            venue: None,
        })
    );
    // another symbol's price, named in the word
    let on_eth = request_of(&[
        "binance",
        "BTCUSDT",
        "buy",
        "1",
        "--at",
        "2099-11-01T12:00:00Z",
        "--when-price",
        "below:ETHUSDT:3000",
    ]);
    assert_eq!(on_eth.release.and_then(|r| r.symbol).as_deref(), Some("ETHUSDT"));
    for (extra, want) in [
        (vec!["--at", "2001-01-01T00:00:00Z"], "--at: the release"),
        (vec!["--at", "1793412600"], "--at 1793412600 looks like epoch SECONDS"),
        (vec!["--at", "soon"], "not a UTC instant"),
        (vec!["--when-price", "above:100"], "--when-price needs --at"),
        (vec!["--at", "2099-11-01T12:00:00Z", "--when-price", "over:1"], "--when-price: "),
        (
            vec![
                "@100",
                "--tif",
                "gtd",
                "--expires",
                "2099-01-01T00:00:00Z",
                "--at",
                "2099-11-01T12:00:00Z",
            ],
            "would expire before it is released",
        ),
    ] {
        let argv: Vec<&str> =
            ["binance", "BTCUSDT", "buy", "1"].into_iter().chain(extra.iter().copied()).collect();
        let e = parse_submit_args(&argv).expect_err(&format!("{argv:?}"));
        assert_eq!(e.exit, Exit::Usage, "{argv:?}");
        assert!(e.message.contains(want), "{argv:?}: wanted /{want}/ in: {}", e.message);
    }
}

/// `--type market-to-limit` is a market order whose remainder rests as a limit at its first fill's price: it goes
/// out as `market_to_limit` with no price and no trigger, and refuses both, as usage errors. The wire word reaches
/// the preview line.
#[test]
fn a_market_to_limit_takes_no_price_and_no_trigger() {
    for spelling in ["market-to-limit", "market_to_limit", "Market-To-Limit"] {
        let o = request_of(&["binance", "BTCUSDT", "buy", "1", "--type", spelling]);
        assert_eq!(
            (o.order_type.as_str(), o.price, o.trigger_price),
            ("market_to_limit", None, None),
            "{spelling}"
        );
    }
    let line = crate::cmd::trade::describe_command(&CommandWire::Submit(request_of(&[
        "binance",
        "BTCUSDT",
        "buy",
        "1",
        "--type",
        "market-to-limit",
    ])));
    assert!(line.contains(" qty=1 market_to_limit "), "{line}");
    for (extra, want) in [
        (vec!["@100"], "--type market-to-limit takes no @<price>"),
        (vec!["--trigger", "9"], "--type market-to-limit takes no --trigger"),
    ] {
        let argv: Vec<&str> = ["binance", "BTCUSDT", "buy", "1", "--type", "market-to-limit"]
            .into_iter()
            .chain(extra.iter().copied())
            .collect();
        let e = parse_submit_args(&argv).expect_err(&format!("{argv:?}"));
        assert_eq!(e.exit, Exit::Usage, "{argv:?}");
        assert!(e.message.contains(want), "{argv:?}: {}", e.message);
    }
}

/// `--type market-to-limit --tif at-open` is the opening auction's market-to-limit (IB `AUC`): it rides the wire
/// with its lifetime word, and the closing auction is refused in the model's words.
#[test]
fn a_market_to_limit_takes_the_opening_auction_and_not_the_closing_one() {
    let argv = |tif| ["ibkr", "AAPL", "buy", "1", "--type", "market-to-limit", "--tif", tif];
    let o = request_of(&argv("at-open"));
    assert_eq!(
        (o.order_type.as_str(), o.time_in_force.as_deref()),
        ("market_to_limit", Some("at-open"))
    );
    let e = parse_submit_args(&argv("at-close"))
        .expect_err("nothing of the session is left for its remainder");
    assert_eq!(e.exit, Exit::Usage);
    assert!(
        e.message.contains("time in force `at-close` belongs to a market or a limit order"),
        "{}",
        e.message
    );
}

// ---- the attributes wave: attributes, a percent trail, a trailing take-profit, an adjustable stop, a price venue

/// `submit --help` names the attributes wave's flags and the feature word they owe an older node.
#[test]
fn the_submit_help_names_the_attributes_waves_flags_and_their_word() {
    let text = help_of(&["order", "submit"]);
    for needle in [
        "--all-or-none",
        "--sweep-to-fill",
        "--discretion",
        "--trail-percent",
        "trailing-take-profit",
        "--adjust-at",
        "--adjust-to",
        "--adjust-trail-percent",
        "--when-price-on",
        "HIDDEN",
        "order-attributes",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
}

/// **A limit order's attributes** ride the wire as one `attributes` (absent when none is named, never an empty
/// one) and reach the preview line; each misuse is refused, before a node is asked, in
/// `vike_model::attributes_refusal`'s words.
#[test]
fn the_attributes_ride_a_limit_order_and_their_misuse_is_refused_in_the_models_words() {
    use vike_model::OrderAttributes;
    let base = ["ibkr", "AAPL", "buy", "5", "@190"];
    let with = |extra: &[&'static str]| {
        let mut argv = base.to_vec();
        argv.extend_from_slice(extra);
        argv
    };
    let o = request_of(&with(&["--all-or-none", "--sweep-to-fill", "--discretion", "0.5"]));
    assert_eq!(
        o.attributes,
        Some(OrderAttributes {
            all_or_none: true,
            sweep_to_fill: true,
            discretionary_amount: Some(0.5)
        })
    );
    assert_eq!(request_of(&base).attributes, None, "no flag, no field");
    let line = crate::cmd::trade::describe_command(&CommandWire::Submit(o));
    assert!(line.contains(" [all-or-none] [sweep-to-fill] [discretion=0.5] "), "{line}");
    for (class, argv, needle) in [
        (
            "on a market order",
            vec!["ibkr", "AAPL", "buy", "5", "--all-or-none"],
            "belongs to a limit order; this is a market order",
        ),
        (
            "all-or-none at once",
            with(&["--all-or-none", "--tif", "ioc"]),
            "all-or-none at once is `fok`",
        ),
        (
            "a sweep that would rest",
            with(&["--sweep-to-fill", "--post-only"]),
            "takes liquidity at once",
        ),
        (
            "an auction",
            with(&["--all-or-none", "--tif", "at-open"]),
            "takes no auction lifetime (`at-open`)",
        ),
        (
            "a zero discretion",
            with(&["--discretion", "0"]),
            "the discretionary amount 0 is not a finite number above zero",
        ),
        (
            "a discretion that is no number",
            with(&["--discretion", "x"]),
            "not an amount: --discretion",
        ),
    ] {
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// **A percent trail and a trailing take-profit**: `--trail-percent` rides the wire in place of `--trail` on the
/// three trailing kinds, and `--type trailing-take-profit` (any spelling) takes a trail or a percent and an
/// optional `--offset`; the preview names the percent. Each misuse is refused before a node is asked:
/// `--trigger` on a trailing take-profit by name, the rest in `vike_model::order_terms_refusal`'s words.
#[test]
fn a_percent_trail_and_a_trailing_take_profit_ride_the_wire() {
    let pct = request_of(&[
        "okx",
        "BTC-USDT-SWAP",
        "sell",
        "1",
        "--type",
        "trailing",
        "--trail-percent",
        "1.5",
    ]);
    assert_eq!(
        (pct.order_type.as_str(), pct.trail, pct.trail_percent, pct.trigger_price),
        ("trailing", None, Some(1.5), None)
    );
    let line = crate::cmd::trade::describe_command(&CommandWire::Submit(pct));
    assert!(line.contains(" trailing trail=1.5% "), "{line}");
    for word in ["trailing-take-profit", "TRAILING_TAKE_PROFIT"] {
        let ttp = request_of(&["ibkr", "AAPL", "sell", "1", "--type", word, "--trail", "2"]);
        assert_eq!(
            (ttp.order_type.as_str(), ttp.trail, ttp.limit_offset, ttp.trigger_price, ttp.price),
            ("trailing_take_profit", Some(2.0), None, None, None),
            "{word}"
        );
    }
    let lit = request_of(&[
        "ibkr",
        "AAPL",
        "sell",
        "1",
        "--type",
        "trailing-take-profit",
        "--trail-percent",
        "2",
        "--offset",
        "0.5",
    ]);
    assert_eq!((lit.trail_percent, lit.limit_offset), (Some(2.0), Some(0.5)));
    assert_eq!(request_of(&["binance", "BTCUSDT", "buy", "1", "@5"]).trail_percent, None);

    let base = ["ibkr", "AAPL", "sell", "1"];
    for (class, extra, needle) in [
        (
            "both distances",
            vec!["--type", "trailing", "--trail", "5", "--trail-percent", "1"],
            "trails by a distance or by a percent, not both",
        ),
        (
            "a percent of 100",
            vec!["--type", "trailing-limit", "--trail-percent", "100", "--offset", "0"],
            "the trail percent 100 of a trailing_limit order is not above 0 and below 100",
        ),
        (
            "a percent that is no number",
            vec!["--type", "trailing", "--trail-percent", "x"],
            "not a percent: --trail-percent",
        ),
        (
            "a percent off the trailing kinds",
            vec!["@100", "--trail-percent", "1"],
            "belong to --type trailing|trailing-limit|trailing-take-profit",
        ),
        (
            "an activation on a trailing take-profit",
            vec!["--type", "trailing-take-profit", "--trail", "2", "--trigger", "9"],
            "--type trailing-take-profit takes no --trigger",
        ),
        (
            "a trailing take-profit with no trail",
            vec!["--type", "trailing-take-profit"],
            "needs a trail distance above zero",
        ),
        (
            "a trailing take-profit with a price",
            vec!["@100", "--type", "trailing-take-profit", "--trail", "2"],
            "takes no price",
        ),
        (
            "a negative offset",
            vec!["--type", "trailing-take-profit", "--trail", "2", "--offset=-1"],
            "is below zero",
        ),
    ] {
        let argv: Vec<&str> = base.iter().copied().chain(extra).collect();
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// **An adjustable stop**: `--adjust-at` with `--adjust-to stop|stop-limit|trailing` and the target's prices
/// becomes the wire's `adjust` (each target, any spelling), and the preview names it. Each misuse is refused
/// before a node is asked: a flag without its partner and a term of another target by the shared reader's
/// words, the numbers in `vike_model::stop_adjust_refusal`'s.
#[test]
fn an_adjustable_stop_rides_the_wire_and_its_misuse_is_refused() {
    use vike_model::{OrderStopAdjust, OrderStopAdjustTo};
    // a sell stop at 90: its adjustment level is above it, the target's stop below that level
    let base = ["okx", "BTC-USDT-SWAP", "sell", "1", "--type", "stop", "--trigger", "90"];
    let with = |extra: &[&'static str]| {
        let mut argv = base.to_vec();
        argv.extend_from_slice(extra);
        argv
    };
    let adjust = |extra: &[&'static str]| request_of(&with(extra)).adjust;
    assert_eq!(
        adjust(&["--adjust-at", "110", "--adjust-to", "stop", "--adjust-stop", "100"]),
        Some(OrderStopAdjust { at: 110.0, to: OrderStopAdjustTo::Stop { stop: 100.0 } })
    );
    assert_eq!(
        adjust(&[
            "--adjust-at=110",
            "--adjust-to",
            "STOP-LIMIT",
            "--adjust-stop",
            "100",
            "--adjust-limit",
            "99",
        ]),
        Some(OrderStopAdjust {
            at: 110.0,
            to: OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 }
        })
    );
    assert_eq!(
        adjust(&[
            "--adjust-at",
            "110",
            "--adjust-to",
            "trailing",
            "--adjust-stop",
            "100",
            "--adjust-trail-percent",
            "2",
        ]),
        Some(OrderStopAdjust {
            at: 110.0,
            to: OrderStopAdjustTo::Trailing { stop: 100.0, trail: None, trail_percent: Some(2.0) }
        })
    );
    assert_eq!(request_of(&base).adjust, None, "no flag, no field");
    let line = crate::cmd::trade::describe_command(&CommandWire::Submit(request_of(&with(&[
        "--adjust-at",
        "110",
        "--adjust-to",
        "trailing",
        "--adjust-stop",
        "100",
        "--adjust-trail",
        "3",
    ]))));
    assert!(line.contains(" [adjust at 110 to trailing from 100 by 3] "), "{line}");

    for (class, argv, needle) in [
        ("a level with no target", with(&["--adjust-at", "110"]), "--adjust-at needs --adjust-to"),
        (
            "a target with no level",
            with(&["--adjust-to", "stop", "--adjust-stop", "100"]),
            "need --adjust-at <price>",
        ),
        (
            "a target with no stop",
            with(&["--adjust-at", "110", "--adjust-to", "stop"]),
            "--adjust-to: an adjustment to `stop` needs its target's stop price",
        ),
        (
            "a target that is none",
            with(&["--adjust-at", "110", "--adjust-to", "sideways", "--adjust-stop", "100"]),
            "is not an adjustment target",
        ),
        (
            "a term of another target",
            with(&[
                "--adjust-at",
                "110",
                "--adjust-to",
                "stop",
                "--adjust-stop",
                "100",
                "--adjust-limit",
                "99",
            ]),
            "takes no limit price",
        ),
        (
            "a level that is no number",
            with(&["--adjust-at", "x", "--adjust-to", "stop", "--adjust-stop", "100"]),
            "not a price: --adjust-at",
        ),
        (
            "a level on the stop's own side",
            with(&["--adjust-at", "80", "--adjust-to", "stop", "--adjust-stop", "85"]),
            "is not on the protected side of the stop 90",
        ),
        (
            "a target that would fire at once",
            with(&["--adjust-at", "110", "--adjust-to", "stop", "--adjust-stop", "120"]),
            "is not on the near side of its level 110",
        ),
        (
            "a trailing target with two distances",
            with(&[
                "--adjust-at",
                "110",
                "--adjust-to",
                "trailing",
                "--adjust-stop",
                "100",
                "--adjust-trail",
                "2",
                "--adjust-trail-percent",
                "1",
            ]),
            "the adjustment's target: a trailing order trails by a distance or by a percent",
        ),
        (
            "an adjustment on a limit",
            vec![
                "okx",
                "BTC-USDT-SWAP",
                "sell",
                "1",
                "@100",
                "--adjust-at",
                "110",
                "--adjust-to",
                "stop",
                "--adjust-stop",
                "100",
            ],
            "an adjustment belongs to a stop or a stop_limit order; this is a limit order",
        ),
    ] {
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// **A condition read on another venue** (decision 0267's 2.13): `--when-price-on` rides the release as typed
/// beside a condition that names its symbol, and the preview says whose price it waits for. It needs
/// `--when-price` (which needs `--at`), a symbol in the word, and a venue id exactly, each refused before a node
/// is asked.
#[test]
fn a_price_venue_rides_the_release_and_its_misuse_is_refused() {
    use vike_tradehub_client::wire::OrderReleasePriceWire;
    let base = ["ibkr", "AAPL", "buy", "1", "@190"];
    let with = |extra: &[&'static str]| {
        let mut argv = base.to_vec();
        argv.extend_from_slice(extra);
        argv
    };
    let at = ["--at", "2099-11-01T12:00:00Z"];
    let o = request_of(&with(&[
        at[0],
        at[1],
        "--when-price",
        "above:BTCUSDT:100000",
        "--when-price-on",
        "binance",
    ]));
    assert_eq!(
        o.release,
        Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePriceWire::Above(100_000.0)),
            symbol: Some("BTCUSDT".into()),
            venue: Some("binance".into()),
        })
    );
    let line = crate::cmd::trade::describe_command(&CommandWire::Submit(o));
    assert!(
        line.contains(
            " [releases at 2099-11-01T12:00:00Z once BTCUSDT on binance is at or above 100000] "
        ),
        "{line}"
    );
    for (class, argv, needle) in [
        (
            "a venue with no condition",
            with(&["--when-price-on", "binance"]),
            "--when-price-on needs --when-price",
        ),
        (
            "a condition with no moment",
            with(&["--when-price", "above:BTCUSDT:1", "--when-price-on", "binance"]),
            "--when-price needs --at",
        ),
        (
            "a condition with no symbol",
            with(&[at[0], at[1], "--when-price", "above:1", "--when-price-on", "binance"]),
            "--when-price-on: a price read on `binance` names its symbol",
        ),
        (
            "a venue that is no roster id",
            with(&[at[0], at[1], "--when-price", "above:BTCUSDT:1", "--when-price-on", "Binance"]),
            "--when-price-on: the price venue `Binance` is not a venue this platform knows",
        ),
    ] {
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// `modify --trigger` names the new trigger price alone or beside the other two, and is a finite number.
#[test]
fn a_modify_names_a_trigger() {
    assert_eq!(
        parse_modify_args(&["c-1", "--trigger", "95"]).expect("a trigger alone is a change"),
        Verb::Modify {
            order_client_id: "c-1".into(),
            new_qty: None,
            new_price: None,
            new_trigger_price: Some(95.0),
        }
    );
    let e = parse_modify_args(&["c-1", "--trigger", "inf"]).expect_err("no finite trigger");
    assert!(e.message.contains("trigger must be a finite number"), "{}", e.message);
    let e = parse_modify_args(&["c-1"]).expect_err("nothing to change");
    assert!(e.message.contains("--qty, --price and/or --trigger"), "{}", e.message);
}

/// The restart-safe-orders drill's protective stop: a reduce-only stop on a LABELLED book. The
/// account rides exactly as a market or limit order's does, and the type and trigger land on the
/// wire fields the node and the venue adapters already read.
#[test]
fn a_stop_names_its_type_and_trigger_and_keeps_the_book() {
    let o = request_of(&[
        "hyperliquid/LIVE",
        "ETH",
        "sell",
        "0.01",
        "--type",
        "stop",
        "--trigger",
        "1800",
        "--reduce-only",
    ]);
    assert_eq!(o.order_type, "stop");
    assert_eq!(o.trigger_price, Some(1800.0));
    assert_eq!(o.price, None, "a stop carries only its trigger");
    assert_eq!(o.account.as_deref(), Some("LIVE"));
    assert!(o.reduce_only);
}

#[test]
fn a_stop_limit_takes_its_limit_price_as_the_at_token() {
    let o = request_of(&[
        "bybit/DEMO",
        "ETHUSDT",
        "sell",
        "1",
        "@1795",
        "--type",
        "stop-limit",
        "--trigger",
        "1800",
    ]);
    assert_eq!(o.order_type, "stop_limit");
    assert_eq!((o.price, o.trigger_price), (Some(1795.0), Some(1800.0)));
}

/// A take-profit with an `@price` is a limit-if-touched: the price is its limit, the trigger what
/// arms it. Without one it is the market-if-touched it always was.
#[test]
fn a_take_profit_takes_an_optional_limit_price_as_the_at_token() {
    let lit = request_of(&[
        "bybit/DEMO",
        "ETHUSDT",
        "sell",
        "1",
        "@2105",
        "--type",
        "take-profit",
        "--trigger",
        "2100",
    ]);
    assert_eq!(lit.order_type, "take_profit");
    assert_eq!((lit.price, lit.trigger_price), (Some(2105.0), Some(2100.0)));
    let mit = request_of(&[
        "bybit/DEMO",
        "ETHUSDT",
        "sell",
        "1",
        "--type",
        "take-profit",
        "--trigger",
        "2100",
    ]);
    assert_eq!((mit.order_type.as_str(), mit.price), ("take_profit", None));
}

/// The hyphenated flag value, the platform's underscore word and any casing all name one kind.
#[test]
fn the_kind_is_read_in_any_casing_and_either_separator() {
    for (word, wire) in [
        ("take-profit", "take_profit"),
        ("take_profit", "take_profit"),
        ("TAKE-PROFIT", "take_profit"),
        ("Stop", "stop"),
    ] {
        let o =
            request_of(&["binance", "BTCUSDT", "sell", "1", "--type", word, "--trigger", "70000"]);
        assert_eq!(o.order_type, wire, "{word}");
    }
}

/// Without the two flags nothing changed: a limit or market order, with no trigger.
#[test]
fn without_the_flags_there_is_no_trigger() {
    assert_eq!(request_of(&["binance", "BTCUSDT", "buy", "1"]).trigger_price, None);
    assert_eq!(request_of(&["binance", "BTCUSDT", "buy", "1", "@5"]).trigger_price, None);
}

#[test]
fn a_trigger_order_that_does_not_hang_together_is_a_usage_error() {
    for (class, argv, needle) in [
        (
            "--type without --trigger",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop"],
            "--type needs --trigger",
        ),
        (
            "--trigger without --type",
            vec!["binance", "BTCUSDT", "sell", "1", "--trigger", "9"],
            "--trigger needs --type",
        ),
        (
            "a kind that is not a trigger kind",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "limit", "--trigger", "9"],
            "not a trigger kind",
        ),
        (
            "stop-limit with no limit price",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop-limit", "--trigger", "9"],
            "needs the limit price",
        ),
        (
            "stop with a price",
            vec!["binance", "BTCUSDT", "sell", "1", "@8", "--type", "stop", "--trigger", "9"],
            "takes no @<price>",
        ),
        (
            "take-profit with a zero limit price",
            vec![
                "binance",
                "BTCUSDT",
                "sell",
                "1",
                "@0",
                "--type",
                "take-profit",
                "--trigger",
                "9",
            ],
            "the limit price 0 of a take_profit order is not above zero",
        ),
        (
            "take-profit with a negative limit price",
            vec![
                "binance",
                "BTCUSDT",
                "sell",
                "1",
                "@-1",
                "--type",
                "take-profit",
                "--trigger",
                "9",
            ],
            "the limit price -1 of a take_profit order is not above zero",
        ),
        (
            "stop-limit with a zero limit price",
            vec!["binance", "BTCUSDT", "sell", "1", "@0", "--type", "stop-limit", "--trigger", "9"],
            "a stop_limit order needs a limit price above zero",
        ),
        (
            "a trigger that is not a number",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop", "--trigger", "high"],
            "not a trigger price",
        ),
        (
            "a trigger that is not finite",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop", "--trigger", "inf"],
            "trigger price must be a finite number",
        ),
        (
            "a zero trigger",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop", "--trigger", "0"],
            "--trigger must be a price above zero",
        ),
        (
            "a negative trigger",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop", "--trigger=-5"],
            "--trigger must be a price above zero",
        ),
        (
            "a NaN trigger",
            vec!["binance", "BTCUSDT", "sell", "1", "--type", "stop", "--trigger", "nan"],
            "--trigger must be a price above zero",
        ),
    ] {
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// **A trailing order**: `--type trailing --trail X` (the activation optional on `--trigger`) and
/// `--type trailing-limit --trail X --offset Y` become the wire's kind word, trail, limit offset and trigger
/// price; any spelling of the kind names it.
#[test]
fn a_trailing_order_carries_its_trail_offset_and_activation() {
    let stop =
        request_of(&["okx", "BTC-USDT-SWAP", "sell", "1", "--type", "trailing", "--trail", "50"]);
    assert_eq!(
        (stop.order_type.as_str(), stop.trail, stop.limit_offset, stop.trigger_price, stop.price),
        ("trailing", Some(50.0), None, None, None)
    );
    for word in ["trailing-limit", "TRAILING_LIMIT"] {
        let limit = request_of(&[
            "ibkr",
            "AAPL",
            "sell",
            "1",
            "--type",
            word,
            "--trail",
            "2",
            "--offset=0.5",
            "--trigger",
            "190",
        ]);
        assert_eq!(
            (limit.order_type.as_str(), limit.trail, limit.limit_offset, limit.trigger_price),
            ("trailing_limit", Some(2.0), Some(0.5), Some(190.0)),
            "{word}"
        );
    }
    let plain = request_of(&["binance", "BTCUSDT", "buy", "1", "@5"]);
    assert_eq!((plain.trail, plain.limit_offset), (None, None), "no flag, no field");
}

/// **A pegged order**: `--type pegged --peg <anchor> [--offset X]` becomes the wire's kind word and peg (the
/// anchor lower-cased, no `--offset` an offset of 0), never a limit offset; anything that does not hang
/// together is a usage error before a node is asked.
#[test]
fn a_pegged_order_carries_its_peg_and_one_that_does_not_hang_together_is_refused() {
    let with = |extra: &[&'static str]| {
        let mut argv = vec!["ibkr", "AAPL", "buy", "1", "--type", "pegged"];
        argv.extend_from_slice(extra);
        argv
    };
    let mid = request_of(&with(&["--peg", "MID", "--offset=-0.05"]));
    assert_eq!(
        (mid.order_type.as_str(), mid.price, mid.trigger_price, mid.limit_offset),
        ("pegged", None, None, None)
    );
    assert_eq!(mid.peg, Some(OrderPegWire { anchor: "mid".into(), offset: -0.05 }));
    let best = request_of(&with(&["--peg", "primary"]));
    assert_eq!(best.peg, Some(OrderPegWire { anchor: "primary".into(), offset: 0.0 }));
    assert_eq!(
        request_of(&["binance", "BTCUSDT", "buy", "1", "@5"]).peg,
        None,
        "no flag, no field"
    );
    for (class, argv, needle) in [
        ("no peg", with(&[]), "--type pegged needs --peg, one of best, mid, market, primary"),
        ("an unknown anchor", with(&["--peg", "near"]), "--peg \"near\" is not an anchor"),
        ("a price", with(&["--peg", "mid", "@100"]), "a pegged order takes no price"),
        ("a trigger", with(&["--peg", "mid", "--trigger", "9"]), "takes no --trigger"),
        ("an offset that is no number", with(&["--peg", "mid", "--offset", "x"]), "not a price"),
        ("a trail", with(&["--peg", "mid", "--trail", "1"]), "belong to --type trailing"),
        (
            "a peg on a limit",
            vec!["ibkr", "AAPL", "buy", "1", "@100", "--peg", "mid"],
            "--peg belongs to --type pegged",
        ),
    ] {
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

#[test]
fn a_trailing_order_that_does_not_hang_together_is_a_usage_error() {
    let base = ["okx", "BTC-USDT-SWAP", "sell", "1"];
    for (class, extra, needle) in [
        ("no trail", vec!["--type", "trailing"], "needs a trail distance above zero"),
        ("a zero trail", vec!["--type", "trailing", "--trail", "0"], "needs a trail distance"),
        ("a trail that is no number", vec!["--type", "trailing", "--trail", "x"], "not a price"),
        (
            "a trailing-limit with no offset",
            vec!["--type", "trailing-limit", "--trail", "5"],
            "needs a limit offset of zero or more",
        ),
        (
            "a negative offset",
            vec!["--type", "trailing-limit", "--trail", "5", "--offset=-1"],
            "is below zero",
        ),
        (
            "an offset on a trailing stop",
            vec!["--type", "trailing", "--trail", "5", "--offset", "1"],
            "belongs to a trailing_limit order",
        ),
        ("a price", vec!["@100", "--type", "trailing", "--trail", "5"], "takes no price"),
        (
            "a zero activation",
            vec!["--type", "trailing", "--trail", "5", "--trigger", "0"],
            "activation price 0",
        ),
        (
            "an activation that is no number",
            vec!["--type", "trailing", "--trail", "5", "--trigger", "x"],
            "not an activation price",
        ),
        (
            "a trail with no trailing kind",
            vec!["@100", "--trail", "5"],
            "belong to --type trailing",
        ),
        (
            "an offset on a stop",
            vec!["--type", "stop", "--trigger", "9", "--offset", "1"],
            "belong to --type trailing",
        ),
    ] {
        let argv: Vec<&str> = base.iter().copied().chain(extra).collect();
        let e = parse_submit_args(&argv).expect_err(class);
        assert_eq!(e.exit, Exit::Usage, "{class}");
        assert!(e.message.contains(needle), "{class}: {}", e.message);
    }
}

/// The line the operator answers `y/N` to must carry the number the order fires at, and a
/// non-trigger order's line must not grow one.
#[test]
fn the_preview_line_shows_the_trigger_price() {
    let describe = |argv: &[&str]| {
        let cmd = submit_of(argv).to_wire_command().expect("a submit is a write");
        crate::cmd::trade::describe_command(&cmd)
    };
    let stop =
        describe(&["binance", "BTCUSDT", "sell", "1", "--type", "stop", "--trigger", "1800"]);
    assert!(stop.contains(" stop trigger=1800 "), "{stop}");
    let limit = describe(&["binance", "BTCUSDT", "sell", "1", "@1795"]);
    assert!(!limit.contains("trigger"), "{limit}");
    assert!(!limit.contains("trail") && !limit.contains("offset"), "{limit}");
    let trailing = describe(&[
        "ibkr",
        "AAPL",
        "sell",
        "1",
        "--type",
        "trailing-limit",
        "--trail",
        "2",
        "--offset",
        "0.5",
        "--trigger",
        "190",
    ]);
    assert!(trailing.contains(" trailing_limit trigger=190 trail=2 offset=0.5 "), "{trailing}");
}

/// **The line the operator answers `y/N` to names the ACCOUNT**, on the order verb and on the three
/// that reduce: the venue alone reads the same for `hyperliquid` and `hyperliquid/LIVE`. `-` is "no
/// account named", not `DEFAULT` (a name the sender chose), as in the bracket and mount lines.
#[test]
fn the_confirmation_line_names_the_account_on_every_order_and_reduce_verb() {
    let describe = crate::cmd::trade::describe_command;
    let submit = |book: &str| {
        let cmd = submit_of(&[book, "ETH", "sell", "1", "--type", "stop", "--trigger", "1800"])
            .to_wire_command()
            .expect("a submit is a write");
        describe(&cmd)
    };
    assert!(
        submit("hyperliquid/LIVE").ends_with(" account=LIVE"),
        "{}",
        submit("hyperliquid/LIVE")
    );
    assert!(
        submit("hyperliquid/DEFAULT").ends_with(" account=DEFAULT"),
        "{}",
        submit("hyperliquid")
    );
    assert!(submit("hyperliquid").ends_with(" account=-"), "{}", submit("hyperliquid"));

    let unnamed = CommandWire::Submit(OrderRequestWire {
        order_client_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "ETH".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    });
    assert_eq!(
        describe(&unnamed),
        "SUBMIT buy hyperliquid ETH qty=1 market order_client_id=c1 account=-",
        "a Submit that names no account says so"
    );

    let (venue, account) = (Some("hyperliquid".to_string()), Some("LIVE".to_string()));
    assert_eq!(
        describe(&CommandWire::Flatten {
            venue: "hyperliquid".into(),
            symbol: "ETH".into(),
            account: account.clone()
        }),
        "FLATTEN hyperliquid ETH account=LIVE"
    );
    assert_eq!(
        describe(&CommandWire::MarketExit { venue: venue.clone(), account: account.clone() }),
        "MARKET-EXIT venue=hyperliquid account=LIVE"
    );
    assert_eq!(
        describe(&CommandWire::MassCancel { venue, symbol: None, account }),
        "MASS-CANCEL venue=hyperliquid symbol=* account=LIVE"
    );
    assert_eq!(
        describe(&CommandWire::MarketExit { venue: None, account: None }),
        "MARKET-EXIT venue=* account=-",
        "the unscoped panic button names no account"
    );
}

/// One row per flag `submit` defines: how it is typed AFTER `--reason` (bare and, for the long
/// flags that take a value, inline `--flag=value`), and the flag the refusal must name.
/// [`oneshot::flags_of`] derives the list from the `clap` definition, so
/// `every_submit_flag_has_a_row_here` fails the day a flag is added without its row.
const SUBMIT_FLAG_ROWS: [(&str, &str); 50] = [
    ("--reduce-only", "--reduce-only"),
    ("--order-client-id", "--order-client-id"),
    ("--order-client-id=abc123", "--order-client-id"),
    ("--type", "--type"),
    ("--type=stop", "--type"),
    ("--trigger", "--trigger"),
    ("--trigger=100", "--trigger"),
    // swallowed, a trailing order would go out without its trail (and so be refused) or its offset
    ("--trail", "--trail"),
    ("--trail=5", "--trail"),
    ("--offset", "--offset"),
    ("--offset=1", "--offset"),
    // swallowed, a pegged order would go out without its peg (and so be refused)
    ("--peg", "--peg"),
    ("--peg=mid", "--peg"),
    // the lifetime: swallowed by `--reason`, a `--post-only` would go out as a plain limit that takes liquidity
    ("--tif", "--tif"),
    ("--tif=ioc", "--tif"),
    ("--expires", "--expires"),
    ("--expires=2030-01-01T00:00:00Z", "--expires"),
    ("--post-only", "--post-only"),
    // swallowed, an iceberg goes out wholly in view and a held order goes out NOW
    ("--display", "--display"),
    ("--display=1", "--display"),
    ("--at", "--at"),
    ("--at=2030-01-01T00:00:00Z", "--at"),
    ("--when-price", "--when-price"),
    ("--when-price=above:1", "--when-price"),
    // swallowed, the condition reads the order's own venue's price
    ("--when-price-on", "--when-price-on"),
    ("--when-price-on=binance", "--when-price-on"),
    // swallowed, a percent trailing order goes out with no trail (and so is refused)
    ("--trail-percent", "--trail-percent"),
    ("--trail-percent=1", "--trail-percent"),
    // swallowed, a limit order goes out without its attribute: it fills in part, or at its price alone
    ("--all-or-none", "--all-or-none"),
    ("--sweep-to-fill", "--sweep-to-fill"),
    ("--discretion", "--discretion"),
    ("--discretion=0.5", "--discretion"),
    // swallowed, a stop goes out that never adjusts
    ("--adjust-at", "--adjust-at"),
    ("--adjust-at=110", "--adjust-at"),
    ("--adjust-to", "--adjust-to"),
    ("--adjust-to=stop", "--adjust-to"),
    ("--adjust-stop", "--adjust-stop"),
    ("--adjust-stop=105", "--adjust-stop"),
    ("--adjust-limit", "--adjust-limit"),
    ("--adjust-limit=104", "--adjust-limit"),
    ("--adjust-trail", "--adjust-trail"),
    ("--adjust-trail=2", "--adjust-trail"),
    ("--adjust-trail-percent", "--adjust-trail-percent"),
    ("--adjust-trail-percent=1", "--adjust-trail-percent"),
    ("--node", "--node"),
    ("--node=h:1", "--node"),
    ("--yes", "--yes"),
    ("-y", "-y"),
    ("--json", "--json"),
    ("--no-wait", "--no-wait"),
];

/// `--reason` takes everything after it, flags included, so ANY flag of `submit` written after it
/// never reaches the order: `--type`/`--trigger` would send a plain MARKET order, and a dropped
/// `--reduce-only` would OPEN risk. Each is refused instead, telling the operator where the flag goes.
#[test]
fn any_submit_flag_after_reason_is_refused_not_swallowed() {
    for (typed, flag) in SUBMIT_FLAG_ROWS {
        let argv = ["binance", "BTCUSDT", "sell", "1", "--reason", "why", typed];
        let e = parse_submit_args(&argv).expect_err(typed);
        assert_eq!(e.exit, Exit::Usage, "{typed}");
        assert!(e.message.contains(&format!("`{flag}`")), "{typed}: {}", e.message);
        assert!(
            e.message.contains(&format!("Put `{flag}` BEFORE --reason")),
            "{typed}: {}",
            e.message
        );
    }
    // The shapes an operator actually types: several flags, one of them the dangerous one.
    for argv in [
        vec!["binance", "BTCUSDT", "sell", "1", "--reason", "why", "--reduce-only", "--yes"],
        vec![
            "binance",
            "BTCUSDT",
            "sell",
            "1",
            "--reason",
            "why",
            "--type",
            "stop",
            "--trigger",
            "100",
            "--yes",
        ],
    ] {
        let e = parse_submit_args(&argv).expect_err("a swallowed flag is refused");
        assert_eq!(e.exit, Exit::Usage, "{argv:?}");
        assert!(e.message.contains("BEFORE --reason"), "{argv:?}: {}", e.message);
    }
}

/// The list the guard reads is the one the `clap` definition gives, and every flag in it has a row
/// in [`SUBMIT_FLAG_ROWS`] (and every row names a real flag). `--reason` itself is left out: a
/// `--reason` inside the text is only more text.
#[test]
fn every_submit_flag_has_a_row_here() {
    let mut derived = oneshot::flags_of::<SubmitCommand>();
    derived.sort();
    let mut rows: Vec<&str> = SUBMIT_FLAG_ROWS.iter().map(|(_, flag)| *flag).collect();
    rows.sort();
    rows.dedup();
    assert_eq!(derived.iter().map(String::as_str).collect::<Vec<_>>(), rows);
    let o = request_of(&["binance", "BTCUSDT", "sell", "1", "--reason", "why --reason again"]);
    assert_eq!(o.order_type, "market");
}

#[test]
fn a_reason_that_only_contains_other_words_is_untouched() {
    // Before --reason the same flags build the trigger order, and a reason that merely contains
    // other words (or the words `type` and `trigger` without dashes) is untouched.
    let o = request_of(&[
        "binance",
        "BTCUSDT",
        "sell",
        "1",
        "--type",
        "stop",
        "--trigger",
        "100",
        "--reason",
        "scale out: type of trigger unchanged",
    ]);
    assert_eq!((o.order_type.as_str(), o.trigger_price), ("stop", Some(100.0)));
    assert_eq!(
        request_of(&["binance", "BTCUSDT", "buy", "1", "--reason", "why"]).trigger_price,
        None
    );
}
