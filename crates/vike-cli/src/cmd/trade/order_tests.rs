use super::*;
use crate::cmd::trade::help_of;

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

#[test]
fn no_verb_names_the_roster() {
    let e = parsed(&[]).unwrap_err();
    assert!(!e.is_help(), "a bare group is a usage error: {e}");
    for verb in ["ls", "submit", "cancel", "modify", "mass-cancel"] {
        assert!(e.contains(verb), "the refusal must name `{verb}`: {e}");
    }
}

#[test]
fn help_short_circuits_before_any_verb_specific_parsing() {
    for spelling in ["-h", "--help", "help"] {
        let e = parsed(&[spelling]).unwrap_err();
        assert!(e.is_help(), "{spelling}: {e}");
    }
}

#[test]
fn an_unknown_verb_names_itself() {
    let e = parsed(&["bogus"]).unwrap_err();
    assert!(!e.is_help(), "{e}");
    assert!(e.contains("bogus"), "{e}");
}

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

#[test]
fn ls_help_short_circuits() {
    let e = parsed(&["ls", "--help"]).unwrap_err();
    assert!(e.is_help(), "{e}");
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

#[test]
fn the_coid_is_minted_before_the_preview_so_the_id_shown_is_the_id_sent() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    match v {
        Verb::Submit(o) => assert!(
            !o.client_order_id.is_empty(),
            "a node REFUSES a remote Submit with an empty client_order_id"
        ),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn a_missing_book_is_a_usage_error_naming_the_forms() {
    let e = parse_submit_args(&["BTCUSDT", "buy", "0.01"]).expect_err("the book is required");
    assert!(e.msg.contains(crate::cmd::trade::selector::FORMS));
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
/// `crates/vike-cli/tests/trade_plane_cli.rs`'s
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

/// The wire's three states ([`WireOrderRequest::account`]) mean this parser must never leave the
/// field `None`: a book was ALWAYS given (it is required), so even the bare, unlabelled spelling
/// names the default account POSITIVELY (`"DEFAULT"`) rather than naming nothing — the
/// distinction the field's own doc argues (absence cannot be trusted across a version boundary).
#[test]
fn a_bare_venue_names_the_default_account_positively() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    match v {
        Verb::Submit(o) => assert_eq!(o.account.as_deref(), Some("DEFAULT")),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn submit_reduce_only_and_pinned_coid_survive_the_book_first_grammar() {
    let v = parse_submit_args(&[
        "okx",
        "BTC-USDT",
        "sell",
        "2",
        "@100",
        "--reduce-only",
        "--coid",
        "MYID001",
    ])
    .unwrap();
    match v {
        Verb::Submit(o) => {
            assert!(o.reduce_only);
            assert_eq!(o.client_order_id, "MYID001");
            assert_eq!(o.side, -1);
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

/// The one-shot verb's half of "a refusal is for an order with NO size, never for a big one": a
/// sizeable submit over EVERY advisory cap still parses, and the preview's guardrail still says OVER
/// LIMIT about it. (The refusal side — the same classes the REPL and the MCP tool refuse, on the
/// usage rung — is `crate::cmd::trade`'s
/// `the_repl_the_one_shot_verb_and_the_mcp_tool_refuse_the_same_unsizeable_orders`, which drives all
/// three parsers off one table.)
#[test]
fn a_sizeable_over_limit_submit_still_parses_and_is_only_advised() {
    let caps = verbs::GuardrailCaps { max_qty: Some(1.0), max_notional: Some(10.0) };
    for argv in [
        vec!["binance", "BTCUSDT", "buy", "5", "@100"], // over the qty cap AND the notional cap
        vec!["binance", "BTCUSDT", "buy", "5"],         // a market order: over the qty cap only
        vec!["binance/ALT", "BTCUSDT", "sell", "1e3", "@1e6", "--reduce-only"], // huge but FINITE
    ] {
        let verb = parse_submit_args(&argv)
            .unwrap_or_else(|e| panic!("{argv:?} must still parse: {}", e.msg));
        let cmd = verb.to_wire_command().expect("a submit is a write");
        let advisory = verbs::guardrail_check(&cmd, caps).line();
        assert!(advisory.contains("OVER LIMIT"), "{argv:?} is advised, not refused: {advisory}");
    }
}

#[test]
fn cancel_takes_exactly_one_coid_and_no_book() {
    let v = parse_cancel_args(&["c-1"]).unwrap();
    assert_eq!(v, Verb::Cancel("c-1".to_string()));
    assert!(parse_cancel_args(&[]).is_err());
    assert!(parse_cancel_args(&["c-1", "extra"]).is_err());
}

#[test]
fn modify_requires_at_least_one_change() {
    let e = parse_modify_args(&["c-1"]).expect_err("nothing to change is an error");
    assert!(e.msg.contains("nothing to change"));
    let v = parse_modify_args(&["c-1", "--qty", "2"]).unwrap();
    assert_eq!(
        v,
        Verb::Modify { client_order_id: "c-1".to_string(), new_qty: Some(2.0), new_price: None }
    );
}

#[test]
fn mass_cancel_book_is_optional_and_names_the_default_account_when_bare() {
    let v = parse_mass_cancel_args(&[]).unwrap();
    assert_eq!(v, Verb::MassCancel { venue: None, symbol: None, account: None });

    let v = parse_mass_cancel_args(&["binance", "BTCUSDT"]).unwrap();
    assert_eq!(
        v,
        Verb::MassCancel {
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            account: Some("DEFAULT".to_string())
        }
    );
}

/// A labelled book on `mass-cancel` is no longer refused HERE — this replaces
/// `mass_cancel_refuses_a_labelled_book_the_node_cannot_route`, which pinned the local refusal
/// stage 5 deleted (see this module's doc). The parser now passes the label through, and what
/// keeps it off a node that would drop it is the CLIENT's capability gate:
/// `crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature` answers
/// `FEATURE_ACCOUNT_SCOPED_REDUCE` for a `MassCancel` naming an account, which no node
/// advertises until it can confine the cancel to that account. That refusal is pinned over a
/// real node by `crates/vike-cli/tests/trade_plane_cli.rs`'s
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

/// A repeated `--node` is last-wins, `-y`/`--yes` may repeat, and `--coid=ID`, `--node=ADDR` and a
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
        "--coid=abc123",
        "--reason",
        "-urgent",
    ])
    .expect("the spellings read") else {
        panic!("expected a submit");
    };
    assert_eq!(c.wrap.node.as_deref(), Some("b:2"), "the last --node wins");
    assert!(c.wrap.yes);
    assert_eq!(c.coid.as_deref(), Some("abc123"));
    assert_eq!(c.wrap.reason().as_deref(), Some("-urgent"));
}

/// An unknown flag is refused by name (it used to fall through as a positional and fail the count),
/// and the `@price` token may sit anywhere among the positionals.
#[test]
fn an_unknown_flag_is_refused_and_the_price_token_floats() {
    let e = parse_submit_args(&["binance", "BTCUSDT", "buy", "1", "--bogus"]).unwrap_err();
    assert!(e.msg.contains("--bogus"), "{}", e.msg);
    let v = parse_submit_args(&["binance", "BTCUSDT", "@100", "buy", "1"]).unwrap();
    match v {
        Verb::Submit(o) => assert_eq!(o.price, Some(100.0)),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

// ---- native trigger orders: `--type` + `--trigger` ----------------------------------------------

fn submit_of(argv: &[&str]) -> Verb {
    parse_submit_args(argv).unwrap_or_else(|e| panic!("{argv:?}: {}", e.msg))
}

fn request_of(argv: &[&str]) -> WireOrderRequest {
    match submit_of(argv) {
        Verb::Submit(o) => o,
        other => panic!("expected a Submit, got {other:?}"),
    }
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
            "take-profit with a price",
            vec![
                "binance",
                "BTCUSDT",
                "sell",
                "1",
                "@8",
                "--type",
                "take-profit",
                "--trigger",
                "9",
            ],
            "takes no @<price>",
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
        assert!(e.msg.contains(needle), "{class}: {}", e.msg);
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
    assert!(submit("hyperliquid").ends_with(" account=DEFAULT"), "{}", submit("hyperliquid"));

    let unnamed = WireCommand::Submit(WireOrderRequest {
        client_order_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "ETH".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        account: None,
    });
    assert_eq!(
        describe(&unnamed),
        "SUBMIT buy hyperliquid ETH qty=1 market coid=c1 account=-",
        "a Submit that names no account says so"
    );

    let (venue, account) = (Some("hyperliquid".to_string()), Some("LIVE".to_string()));
    assert_eq!(
        describe(&WireCommand::Flatten {
            venue: "hyperliquid".into(),
            symbol: "ETH".into(),
            account: account.clone()
        }),
        "FLATTEN hyperliquid ETH account=LIVE"
    );
    assert_eq!(
        describe(&WireCommand::MarketExit { venue: venue.clone(), account: account.clone() }),
        "MARKET-EXIT venue=hyperliquid account=LIVE"
    );
    assert_eq!(
        describe(&WireCommand::MassCancel { venue, symbol: None, account }),
        "MASS-CANCEL venue=hyperliquid symbol=* account=LIVE"
    );
    assert_eq!(
        describe(&WireCommand::MarketExit { venue: None, account: None }),
        "MARKET-EXIT venue=* account=-",
        "the unscoped panic button names no account"
    );
}

/// One row per flag `submit` defines: how it is typed AFTER `--reason` (bare and, for the long
/// flags that take a value, inline `--flag=value`), and the flag the refusal must name.
/// [`submit_flags`] is derived from the `clap` definition, so
/// `every_submit_flag_has_a_row_here` fails the day a flag is added without its row.
const SUBMIT_FLAG_ROWS: [(&str, &str); 12] = [
    ("--reduce-only", "--reduce-only"),
    ("--coid", "--coid"),
    ("--coid=abc123", "--coid"),
    ("--type", "--type"),
    ("--type=stop", "--type"),
    ("--trigger", "--trigger"),
    ("--trigger=100", "--trigger"),
    ("--node", "--node"),
    ("--node=h:1", "--node"),
    ("--yes", "--yes"),
    ("-y", "-y"),
    ("--json", "--json"),
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
        assert!(e.msg.contains(&format!("`{flag}`")), "{typed}: {}", e.msg);
        assert!(e.msg.contains(&format!("Put `{flag}` BEFORE --reason")), "{typed}: {}", e.msg);
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
        assert!(e.msg.contains("BEFORE --reason"), "{argv:?}: {}", e.msg);
    }
}

/// The list the guard reads is the one the `clap` definition gives, and every flag in it has a row
/// in [`SUBMIT_FLAG_ROWS`] (and every row names a real flag). `--reason` itself is left out: a
/// `--reason` inside the text is only more text.
#[test]
fn every_submit_flag_has_a_row_here() {
    let mut derived = submit_flags();
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
