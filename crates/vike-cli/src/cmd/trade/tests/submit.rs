//! Why a write is refused, the REPL's order grammar, and the unsizeable orders all three refuse.

use super::*;
use std::assert_matches;

// -- why writes are disabled: four causes, four messages ---------------------------------------

/// **The reported defect.** With `tradehub_control` off at the node and a VALID key, a write
/// printed `writes disabled (OBSERVE-ONLY): no <key> — nothing was sent`. The key was
/// present and fine; the node had refused the SCOPE, and had said so — once, at connect time,
/// before scrolling away. The operator was sent hunting for a key that was never the problem.
#[test]
fn a_scope_refusal_never_reports_itself_as_a_missing_key() {
    let denied = HubCallError::AccessDenied(
        "tradehub control auth denied: control disabled on this node".into(),
    );
    let cause = NoControl::from_connect_error(&denied);
    assert_eq!(
        cause,
        NoControl::Refused(
            "error [hub.access.denied]: tradehub control auth denied: control disabled on this node"
                .into()
        )
    );

    let line = cause.refusal_line();
    assert_eq!(line.matches("error [hub.access.denied]: ").count(), 1, "{line}");
    assert_eq!(line.matches("error [").count(), 1, "ONE id: {line}");
    assert!(
        !line.contains(&format!("no {}", hubkeys::DEVICE_KEY_WORDS)),
        "the message must not say the key is absent when the key was presented: {line}"
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
    assert!(line.contains(hubkeys::DEVICE_KEY_WORDS), "{line}");
    assert!(line.contains("settings database"), "it must name the place we looked: {line}");
    assert!(!line.contains("environment"), "the environment is not a source: {line}");
    assert!(line.contains("nothing was sent"), "{line}");
}

/// A transport failure is neither of the above: nothing is known about the key or the node's
/// policy, so the message claims neither. Classified by VARIANT, not by message text — the
/// node is free to reword its reason, and `AccessDenied` is the one variant the handshake
/// maps `Response::AuthDenied` to.
#[test]
fn an_unreachable_node_blames_neither_the_key_nor_the_node_policy() {
    let refused = HubCallError::NotSent(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "Connection refused",
    ));
    let cause = NoControl::from_connect_error(&refused);
    assert_matches!(cause, NoControl::Unreachable(_), "{cause:?}");

    let line = cause.refusal_line();
    assert!(!line.contains(&format!("no {}", hubkeys::DEVICE_KEY_WORDS)), "{line}");
    assert!(!line.contains("tradehub_control"), "nothing is known about the node's flag: {line}");
    assert!(line.contains("Connection refused") && line.contains("nothing was sent"), "{line}");
}

/// **An older node or a version skew on the control connect is the node ANSWERING**: it is told to
/// upgrade a side, never to turn the node's flag on, and it exits 1 like a refused scope, where it used
/// to read as unreachable and exit 3, the rung a wrapper retries forever.
#[test]
fn an_older_node_or_a_skew_is_incompatible_and_exits_failed() {
    let older = HubCallError::NotOffered {
        feature: "person-auth".into(),
        text: "this hub does not advertise `person-auth` (an older hub)".into(),
    };
    let skew = HubCallError::Protocol(
        "tradehub protocol version mismatch: client speaks 5, server speaks 4".into(),
    );
    for e in [older, skew] {
        let cause = NoControl::from_connect_error(&e);
        assert_eq!(cause, NoControl::Incompatible(e.to_string()));
        assert_eq!(cause.exit(), crate::exit::Exit::Failed, "{e:?}");
        assert_eq!(
            cause.refusal_line(),
            format!(
                "writes disabled: the node answered the control connect, but this build cannot \
                 use it ({e}) — upgrade the node or this box, whichever is older — nothing was sent"
            )
        );
    }
    let denied = NoControl::from_connect_error(&HubCallError::AccessDenied("no".into()));
    assert_eq!(denied.exit(), crate::exit::Exit::Failed);
    let dial = HubCallError::NotSent(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
    assert_eq!(NoControl::from_connect_error(&dial).exit(), crate::exit::Exit::Connect);
}

/// The four are genuinely different ACTIONS — put a key in the store / turn the node's flag on /
/// upgrade a side / fix the network — so no two may print the same line.
#[test]
fn the_four_causes_print_four_different_lines() {
    let lines = [
        NoControl::NoKey.refusal_line(),
        NoControl::Refused("control disabled on this node".into()).refusal_line(),
        NoControl::Incompatible("version mismatch".into()).refusal_line(),
        NoControl::Unreachable("Connection refused".into()).refusal_line(),
    ];
    for (i, a) in lines.iter().enumerate() {
        for b in lines.iter().skip(i + 1) {
            assert_ne!(a, b, "two distinct causes print the same message");
        }
    }
}

fn submit(v: Verb) -> OrderRequestWire {
    match v {
        Verb::Submit(o) => o,
        other => panic!("expected Submit, got {other:?}"),
    }
}

/// The REPL line takes the one-shot verb's wave-2 flags by its own functions: `--display`, `--at`,
/// `--when-price` (spaced or `=`), `--type market-to-limit` and an auction `--time-in-force`, and refuses their
/// misuse in the same words.
#[test]
fn the_repl_submit_takes_the_display_the_release_and_a_market_to_limit() {
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire};
    let o = submit(
        parse_line(
            "submit binance BTCUSDT buy 5 @100 --display 1 --at 2099-11-01T12:00:00Z \
             --when-price=below:90",
        )
        .unwrap(),
    );
    assert_eq!(o.display_quantity, Some(1.0));
    assert_eq!(
        o.release,
        Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePriceWire::Below(90.0)),
            symbol: None,
            venue: None,
        })
    );
    let mtl = submit(parse_line("submit binance BTCUSDT buy 1 --type market-to-limit").unwrap());
    assert_eq!(
        (mtl.order_type.as_str(), mtl.price, mtl.trigger_price),
        ("market_to_limit", None, None)
    );
    let auction =
        submit(parse_line("submit binance BTCUSDT buy 1 --time-in-force at-close").unwrap());
    assert_eq!(auction.time_in_force.as_deref(), Some("at-close"));
    for (line, want) in [
        ("submit binance BTCUSDT buy 5 --display 1", "belongs to a limit order"),
        ("submit binance BTCUSDT buy 5 @100 --display=5", "hides nothing"),
        ("submit binance BTCUSDT buy 1 --when-price above:1", "--when-price needs --at"),
        (
            "submit binance BTCUSDT buy 1 --at 2001-01-01T00:00:00Z",
            "error [order.submit.release]: --at: the release",
        ),
        ("submit binance BTCUSDT buy 1 --type market-to-limit @100", "takes no @<price>"),
        ("submit binance BTCUSDT buy 1 --type market-to-limit --trigger 9", "takes no --trigger"),
        ("submit binance BTCUSDT buy 1 --at", "--at needs a value"),
    ] {
        let e = parse_line(line).expect_err(line);
        assert!(e.contains(want), "{line}: {e}");
    }
}

/// The REPL line takes the one-shot verb's trailing flags by its own functions: `--trail` and `--offset`
/// (spaced or `=`) beside `--type trailing|trailing-limit` and the optional activation on `--trigger`, and
/// refuses their misuse in the same words.
#[test]
fn the_repl_submit_takes_a_trailing_order() {
    let o = submit(
        parse_line(
            "submit okx BTC-USDT-SWAP sell 1 --type trailing-limit --trail 50 --offset=5 --trigger \
             70000",
        )
        .unwrap(),
    );
    assert_eq!(
        (o.order_type.as_str(), o.trail, o.limit_offset, o.trigger_price),
        ("trailing_limit", Some(50.0), Some(5.0), Some(70000.0))
    );
    let stop = submit(parse_line("sell okx BTC-USDT-SWAP 1 --type trailing --trail=50").unwrap());
    assert_eq!(
        (stop.order_type.as_str(), stop.trail, stop.trigger_price),
        ("trailing", Some(50.0), None)
    );
    for (line, want) in [
        ("submit okx BTC-USDT-SWAP sell 1 --type trailing", "needs a trail distance above zero"),
        ("submit okx BTC-USDT-SWAP sell 1 @100 --trail 5", "a trail belongs to a trailing"),
        ("submit okx BTC-USDT-SWAP sell 1 --type trailing --trail", "--trail needs a value"),
    ] {
        let e = parse_line(line).expect_err(line);
        assert!(e.contains(want), "{line}: {e}");
    }
}

/// The REPL line takes the one-shot verb's pegged order by its own functions: `--peg` (spaced or `=`) and the
/// peg's `--offset` beside `--type pegged`, and refuses its misuse in the same words.
#[test]
fn the_repl_submit_takes_a_pegged_order() {
    use vike_tradehub_client::wire::OrderPegWire;
    let o =
        submit(parse_line("buy ibkr AAPL 1 --type pegged --peg=market --offset -0.02").unwrap());
    assert_eq!((o.order_type.as_str(), o.limit_offset), ("pegged", None));
    assert_eq!(o.peg, Some(OrderPegWire { anchor: "market".into(), offset: -0.02 }));
    for (line, want) in [
        ("buy ibkr AAPL 1 --type pegged", "a pegged order needs its peg"),
        ("buy ibkr AAPL 1 @100 --peg mid", "a peg belongs to a pegged order"),
        ("buy ibkr AAPL 1 --type pegged --peg", "--peg needs a value"),
    ] {
        let e = parse_line(line).expect_err(line);
        assert!(e.contains(want), "{line}: {e}");
    }
}

/// A `submit` line read by BOTH grammars: the REPL's `parse_line` and the one-shot verb over the same tokens
/// (the book first in both). They must build the same order, the order client id aside (the one-shot verb mints its own;
/// the session mints the REPL's in `run_write`).
fn read_by_both(line: &str) -> OrderRequestWire {
    let repl = submit(parse_line(line).unwrap_or_else(|e| panic!("{line}: {e}")));
    let argv: Vec<&str> = line.split_whitespace().skip(1).collect();
    let one_shot =
        super::order::parse_submit_args(&argv).unwrap_or_else(|e| panic!("{line}: {}", e.message));
    let one_shot = submit(one_shot);
    assert_eq!(repl, OrderRequestWire { order_client_id: String::new(), ..one_shot }, "{line}");
    repl
}

/// **The attributes wave's flags at the prompt** (decision 0267): the attributes, a percent trail, a trailing
/// take-profit, an adjustable stop, a price venue and the opening auction's market-to-limit each ride the wire
/// as the one-shot verb builds them, spaced or `=`, and each misuse is refused in the one-shot verb's exact
/// words, since both grammars call the same helpers and `order::submit_rules_usage`.
#[test]
fn the_repl_submit_takes_the_attributes_waves_flags_as_the_one_shot_verb_does() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    let o =
        read_by_both("submit ibkr AAPL buy 5 @190 --all-or-none --sweep-to-fill --discretion=0.5");
    assert_eq!(
        o.attributes,
        Some(OrderAttributes {
            all_or_none: true,
            sweep_to_fill: true,
            discretionary_amount: Some(0.5)
        })
    );
    let o = read_by_both("submit okx BTC-USDT-SWAP sell 1 --type trailing --trail-percent 1.5");
    assert_eq!((o.trail, o.trail_percent), (None, Some(1.5)));
    let o =
        read_by_both("submit ibkr AAPL sell 1 --type trailing-take-profit --trail=2 --offset 0.5");
    assert_eq!(
        (o.order_type.as_str(), o.trail, o.limit_offset),
        ("trailing_take_profit", Some(2.0), Some(0.5))
    );
    let o = read_by_both(
        "submit ibkr AAPL sell 1 --type stop --trigger 180 --adjust-at 200 --adjust-to trailing \
         --adjust-stop=195 --adjust-trail-percent 1",
    );
    assert_eq!(
        o.adjust,
        Some(OrderStopAdjust {
            at: 200.0,
            to: OrderStopAdjustTo::Trailing { stop: 195.0, trail: None, trail_percent: Some(1.0) }
        })
    );
    let o = read_by_both(
        "submit ibkr AAPL sell 1 @179 --type stop-limit --trigger 180 --adjust-at=200 --adjust-to \
         stop-limit --adjust-stop 195 --adjust-limit=194",
    );
    assert_eq!(
        o.adjust.map(|a| a.to),
        Some(OrderStopAdjustTo::StopLimit { stop: 195.0, limit: 194.0 })
    );
    let o = read_by_both(
        "submit ibkr AAPL buy 1 @190 --at 2099-11-01T12:00:00Z --when-price above:BTCUSDT:100000 \
         --when-price-on=binance",
    );
    let release = o.release.expect("a release");
    assert_eq!(
        (release.symbol.as_deref(), release.venue.as_deref()),
        (Some("BTCUSDT"), Some("binance"))
    );
    let o = read_by_both("submit ibkr AAPL buy 1 --type market-to-limit --time-in-force at-open");
    assert_eq!(
        (o.order_type.as_str(), o.time_in_force.as_deref()),
        ("market_to_limit", Some("at-open"))
    );
    assert_eq!(read_by_both("submit ibkr AAPL buy 5 @190 --display 0").display_quantity, Some(0.0));
    // the buy/sell sugar takes them too
    let sugar = submit(
        parse_line("sell ibkr AAPL 1 --type trailing-take-profit --trail-percent 2").unwrap(),
    );
    assert_eq!(
        (sugar.order_type.as_str(), sugar.trail_percent),
        ("trailing_take_profit", Some(2.0))
    );

    for (line, needle) in [
        ("submit ibkr AAPL buy 1 --all-or-none", "belongs to a limit order"),
        (
            "submit ibkr AAPL buy 1 @190 --all-or-none --time-in-force fok",
            "all-or-none at once is `fok`",
        ),
        ("submit ibkr AAPL buy 1 @190 --sweep-to-fill --post-only", "takes liquidity at once"),
        ("submit ibkr AAPL buy 1 @190 --discretion 0", "is not a finite number above zero"),
        ("submit ibkr AAPL sell 1 --type trailing --trail 5 --trail-percent 1", "not both"),
        (
            "submit ibkr AAPL sell 1 --type trailing-take-profit --trail 2 --trigger 9",
            "takes no --trigger",
        ),
        (
            "submit ibkr AAPL sell 1 --type stop --trigger 180 --adjust-at 170 --adjust-to stop \
             --adjust-stop 175",
            "is not on the protected side of the stop 180",
        ),
        (
            "submit ibkr AAPL sell 1 --type stop --trigger 180 --adjust-at 200",
            "--adjust-at needs --adjust-to",
        ),
        (
            "submit ibkr AAPL buy 1 @190 --at 2099-11-01T12:00:00Z --when-price above:1 \
             --when-price-on binance",
            "names its symbol",
        ),
        (
            "submit ibkr AAPL buy 1 @190 --when-price-on binance",
            "--when-price-on needs --when-price",
        ),
        (
            "submit ibkr AAPL buy 1 --type market-to-limit --time-in-force at-close",
            "belongs to a market or a limit order",
        ),
    ] {
        let repl = parse_line(line).expect_err(line);
        let argv: Vec<&str> = line.split_whitespace().skip(1).collect();
        let one_shot = super::order::parse_submit_args(&argv).expect_err(line).message;
        assert_eq!(repl, one_shot, "{line}");
        assert!(repl.starts_with("submit: ") && repl.contains(needle), "{line}: {repl}");
    }
    let e = parse_line("submit ibkr AAPL sell 1 --type stop --trigger 180 --adjust-at")
        .expect_err("no value");
    assert!(e.contains("--adjust-at needs a value"), "{e}");
}

/// **The prompt's `--reason` guard knows every flag the one-shot `submit` defines** (its wrapper's `--node`,
/// `--yes`, ... aside, which this line does not take): a flag one grammar gains and the other's table lacks
/// would go out swallowed at the prompt, the order without it.
#[test]
fn every_one_shot_submit_flag_is_refused_after_reason_at_the_prompt_too() {
    use super::oneshot::{WrapperArgs, flags_of};
    let wrapper = flags_of::<WrapperArgs>();
    for flag in
        flags_of::<super::order::SubmitCommand>().into_iter().filter(|f| !wrapper.contains(f))
    {
        let line = format!("submit binance BTCUSDT buy 1 @100 --reason why {flag}");
        let (cmd, reason) = split_reason(&line);
        let msg = super::repl::grammar_orders::swallowed_flag(cmd, reason.as_deref())
            .unwrap_or_else(|| panic!("{flag}: swallowed at the prompt"));
        assert!(msg.contains(&format!("Put `{flag}` BEFORE --reason")), "{flag}: {msg}");
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
    // below and `verbs::fill_order_client_id`). Keeping the parser pure is what makes every
    // grammar test here deterministic.
    assert!(o.order_client_id.is_empty());
}

/// The REPL takes the lifetime flags by the one-shot verb's own rules (`order::lifetime_flags`): the same
/// wire word, the old frame for the default, and a lifetime flag typed after `--reason` is refused, because
/// swallowed there a maker-only order would go out as a plain limit that takes liquidity.
#[test]
fn the_repl_takes_the_lifetime_flags() {
    let o = submit(parse_line("submit okx BTC-USDT-SWAP buy 1 @100 --post-only").unwrap());
    assert_eq!((o.time_in_force.as_deref(), o.gtd_expiry), (Some("post-only"), None));
    let o = submit(parse_line("buy binance BTCUSDT 1 @100 --time-in-force ioc").unwrap());
    assert_eq!(o.time_in_force.as_deref(), Some("ioc"));
    let o = submit(
        parse_line(
            "sell binance BTCUSDT.P 1 @100 --time-in-force=gtd --expires=2099-11-01T12:00:00Z",
        )
        .unwrap(),
    );
    assert_eq!(o.time_in_force.as_deref(), Some("gtd"));
    assert_eq!(o.gtd_expiry, vike_model::parse_utc_timestamp_ms("2099-11-01T12:00:00Z"));
    let o = submit(parse_line("submit binance BTCUSDT buy 1 @100").unwrap());
    assert_eq!((o.time_in_force, o.gtd_expiry), (None, None), "the old frame");
    for bad in [
        "submit okx BTC-USDT-SWAP buy 1 @100 --post-only --time-in-force ioc",
        "submit okx BTC-USDT-SWAP buy 1 --post-only",
        "submit binance BTCUSDT buy 1 @100 --time-in-force",
        "submit binance BTCUSDT buy 1 @100 --time-in-force gtd",
        "submit binance BTCUSDT buy 1 @100 --time-in-force forever",
    ] {
        assert!(parse_line(bad).is_err(), "{bad}: must be refused");
    }
    for line in [
        "submit okx BTC-USDT-SWAP buy 1 @100 --reason why --post-only",
        "submit binance BTCUSDT buy 1 @100 --reason why --time-in-force ioc",
        "submit binance BTCUSDT buy 1 @100 --reason why --expires=2099-01-01T00:00:00Z",
    ] {
        let (cmd, reason) = split_reason(line);
        let msg = super::repl::grammar_orders::swallowed_flag(cmd, reason.as_deref())
            .unwrap_or_else(|| panic!("{line}: must be refused"));
        assert!(msg.contains("BEFORE --reason"), "{line}: {msg}");
    }
}

/// **THE defect, at this surface.** A parsed `submit` carries no order client id, and the node REFUSES a
/// remote submit with an empty one — so what actually goes on the wire must be the MINTED
/// command. This pins the exact composition `run_write` performs (mint, then describe), which
/// is the step that was missing entirely.
#[test]
fn a_submit_reaches_the_wire_with_a_minted_venue_valid_order_client_id() {
    let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
    let CommandWire::Submit(unminted) = &cmd else { panic!("expected Submit") };
    assert!(unminted.order_client_id.is_empty(), "the parser mints nothing");

    let mut order_client_ids = verbs::order_client_id_minter(OrderClientIdSource::Cli);
    let sent = verbs::fill_order_client_id(cmd, &mut order_client_ids, None, |_| true);
    let CommandWire::Submit(o) = &sent else { panic!("still a Submit") };
    assert!(
        vike_model::is_valid_crypto_order_client_id(&o.order_client_id),
        "the node refuses an empty order client id and the venue refuses a non-alphanumeric one: {:?}",
        o.order_client_id
    );
    // A v2 id the CLI made: `C`, no strategy, no account (no frame), and it opens long.
    assert!(o.order_client_id.starts_with("C000000"), "{:?}", o.order_client_id);
    assert!(o.order_client_id.ends_with("OL"), "{:?}", o.order_client_id);
    // …and the operator can SEE it: the preview line carries the handle they type back.
    let preview = describe_command(&sent);
    assert!(
        preview.contains(&format!("order_client_id={}", o.order_client_id)),
        "the preview must show the id that is sent: {preview}"
    );
}

/// `--order-client-id` pins an id (both flag forms), and an unusable one is a clean parse error rather
/// than a silent fallback to a minted id the caller did not ask for.
#[test]
fn order_client_id_override_pins_the_id_and_rejects_an_unusable_one() {
    for line in [
        "submit sim BTCUSDT buy 1 @100 --order-client-id abc123",
        "buy sim BTCUSDT 1 --order-client-id=abc123",
    ] {
        let o = submit(parse_line(line).unwrap());
        assert_eq!(o.order_client_id, "abc123", "{line}");
    }
    // A pinned id survives the mint untouched — that is what "pin" means.
    let cmd =
        parse_line("submit sim BTCUSDT buy 1 --order-client-id abc123").unwrap().to_wire_command();
    let mut order_client_ids = verbs::order_client_id_minter(OrderClientIdSource::Cli);
    let CommandWire::Submit(o) =
        verbs::fill_order_client_id(cmd.unwrap(), &mut order_client_ids, None, |_| true)
    else {
        panic!("still a Submit")
    };
    assert_eq!(o.order_client_id, "abc123");

    assert!(parse_line("submit sim BTCUSDT buy 1 --order-client-id").is_err()); // dangling value
    assert!(parse_line("submit sim BTCUSDT buy 1 --order-client-id=").is_err()); // empty is not a pin
    assert!(parse_line("submit sim BTCUSDT buy 1 --order-client-id has space").is_err()); // eats a positional
    assert!(parse_line("submit sim BTCUSDT buy 1 --order-client-id c-1").is_err()); // '-' is not venue-safe
    assert!(
        parse_line(&format!("submit sim B buy 1 --order-client-id {}", "x".repeat(33))).is_err()
    );
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

// -- the REPL's `submit` takes the one-shot verb's two pieces: a trigger order and an account --------

/// The restart-safe-orders drill's protective stop, typed at the prompt: a reduce-only stop on a
/// LABELLED book. The type and the trigger land on the wire fields the one-shot verb fills, and the
/// account rides as it does there.
#[test]
fn the_repl_submit_takes_a_trigger_order_and_a_named_account() {
    let o = submit(
        parse_line(
            "submit hyperliquid/LIVE ETH sell 0.01 --type stop --trigger 1800 --reduce-only",
        )
        .unwrap(),
    );
    assert_eq!((o.venue.as_str(), o.account.as_deref()), ("hyperliquid", Some("LIVE")));
    assert_eq!((o.order_type.as_str(), o.trigger_price), ("stop", Some(1800.0)));
    assert_eq!(o.price, None, "a stop carries only its trigger");
    assert!(o.reduce_only);

    // stop-limit also takes the limit price; `--flag=value` and the buy/sell sugar work as for order client id.
    let o = submit(
        parse_line("sell bybit/DEMO ETHUSDT 1 @1795 --type=stop-limit --trigger=1800").unwrap(),
    );
    assert_eq!(
        (o.order_type.as_str(), o.price, o.trigger_price),
        ("stop_limit", Some(1795.0), Some(1800.0))
    );
    assert_eq!((o.venue.as_str(), o.account.as_deref(), o.side), ("bybit", Some("DEMO"), -1));

    let o = submit(
        parse_line("submit binance BTCUSDT sell 1 --type take-profit --trigger 70000").unwrap(),
    );
    assert_eq!((o.order_type.as_str(), o.trigger_price), ("take_profit", Some(70000.0)));
    assert_eq!(o.account, None, "a bare venue names no account");
}

/// A take-profit with an `@price` is a limit-if-touched at the prompt too, and a trigger kind's
/// `@<price>` at zero or below is refused in the one-shot verb's EXACT words: both grammars call
/// `order::submit_rules_usage`.
#[test]
fn the_repl_takes_a_limit_if_touched_and_refuses_a_bad_limit_in_the_one_shot_words() {
    let o = submit(
        parse_line("submit bybit/DEMO ETHUSDT sell 1 @2105 --type take-profit --trigger 2100")
            .unwrap(),
    );
    assert_eq!(
        (o.order_type.as_str(), o.price, o.trigger_price),
        ("take_profit", Some(2105.0), Some(2100.0))
    );
    for (px, kind) in [("@0", "take-profit"), ("@-1", "take-profit"), ("@0", "stop-limit")] {
        let line = format!("submit binance BTCUSDT sell 1 {px} --type {kind} --trigger 9");
        let argv = ["binance", "BTCUSDT", "sell", "1", px, "--type", kind, "--trigger", "9"];
        let repl = parse_line(&line).expect_err(&line);
        let one_shot = super::order::parse_submit_args(&argv).expect_err(&line).message;
        assert_eq!(repl, one_shot, "{line}");
        assert!(repl.starts_with("submit: ") && repl.contains("limit price"), "{line}: {repl}");
    }
}

/// **The REPL reads the book as the one-shot verb does** (decision 0254): the same words build the same
/// order on both surfaces, a bare venue naming no account, and the node's door judges that. The book's
/// refusals are the selector's.
#[test]
fn the_repl_submit_names_the_account_the_one_shot_verb_names() {
    for (line, argv, account) in [
        ("submit binance BTCUSDT buy 1", ["binance", "BTCUSDT", "buy", "1"], None),
        (
            "submit binance/DEFAULT BTCUSDT buy 1",
            ["binance/DEFAULT", "BTCUSDT", "buy", "1"],
            Some("DEFAULT"),
        ),
        ("submit binance/ALT BTCUSDT buy 1", ["binance/ALT", "BTCUSDT", "buy", "1"], Some("ALT")),
        ("buy binance BTCUSDT 1", ["binance", "BTCUSDT", "buy", "1"], None),
    ] {
        let repl = submit(parse_line(line).unwrap());
        let one_shot = submit(super::order::parse_submit_args(&argv).unwrap());
        assert_eq!(repl.venue, one_shot.venue, "{line}");
        assert_eq!(repl.account, one_shot.account, "{line}");
        assert_eq!(repl.account.as_deref(), account, "{line}");
        assert_eq!(repl.venue, "binance", "{line}");
        assert_eq!(repl.trigger_price, None, "{line}");
    }
    let refused = parse_line("buy binance#ALT BTCUSDT 1").expect_err("a route key is no book");
    assert!(refused.starts_with("submit: ") && refused.contains("`binance/ALT`"), "{refused}");
}

/// **The REPL's reducing verbs take a book too**, since a bare venue on a venue of several accounts is now
/// refused and this prompt must have a way to name one. No venue stays the node-wide panic button.
#[test]
fn the_repl_reducing_verbs_read_the_book_as_the_one_shot_verbs_do() {
    let account = |line: &str| match parse_line(line).unwrap() {
        Verb::Flatten { venue, account, .. } => (Some(venue), account),
        Verb::MarketExit { venue, account } | Verb::MassCancel { venue, account, .. } => {
            (venue, account)
        }
        other => panic!("{line}: {other:?}"),
    };
    let named = |account: &str| (Some("hyperliquid".to_string()), Some(account.to_string()));
    for verb in ["flatten %s BTC", "close-all %s", "mass-cancel %s", "mass-cancel %s BTC"] {
        let line = |book: &str| verb.replace("%s", book);
        assert_eq!(
            account(&line("hyperliquid")),
            (Some("hyperliquid".to_string()), None),
            "{verb}"
        );
        assert_eq!(account(&line("hyperliquid/LIVE")), named("LIVE"), "{verb}");
        assert_eq!(account(&line("hyperliquid/DEFAULT")), named("DEFAULT"), "{verb}");
        let refused = parse_line(&line("hyperliquid#LIVE")).expect_err("a route key is no book");
        assert!(refused.contains("`hyperliquid/LIVE`"), "{verb}: {refused}");
    }
    assert_eq!(account("close-all"), (None, None), "the panic button");
    assert_eq!(account("mass-cancel"), (None, None), "and its sibling");
}

/// The refusals are the one-shot verb's own (the same function, so the same words): one flag without
/// the other, a kind that is no trigger kind, a stop with a price, a stop-limit without one, a trigger
/// that is zero, negative, NaN, infinite or not a number, and a flag with no value after it. A bad
/// book is the selector's refusal.
#[test]
fn the_repl_submit_refuses_what_the_one_shot_verb_refuses() {
    for (class, line, needle) in [
        (
            "--type without --trigger",
            "submit binance BTCUSDT sell 1 --type stop",
            "--type needs --trigger",
        ),
        (
            "--trigger without --type",
            "submit binance BTCUSDT sell 1 --trigger 9",
            "--trigger needs --type",
        ),
        (
            "a kind that is not a trigger kind",
            "submit binance BTCUSDT sell 1 --type limit --trigger 9",
            "not a trigger kind",
        ),
        (
            "stop-limit with no limit price",
            "submit binance BTCUSDT sell 1 --type stop-limit --trigger 9",
            "needs the limit price",
        ),
        (
            "stop with a price",
            "submit binance BTCUSDT sell 1 @8 --type stop --trigger 9",
            "takes no @<price>",
        ),
        (
            "a trigger that is not a number",
            "submit binance BTCUSDT sell 1 --type stop --trigger high",
            "not a trigger price",
        ),
        (
            "a trigger that is not finite",
            "submit binance BTCUSDT sell 1 --type stop --trigger inf",
            "trigger price must be a finite number",
        ),
        (
            "a zero trigger",
            "submit binance BTCUSDT sell 1 --type stop --trigger 0",
            "--trigger must be a price above zero",
        ),
        (
            "a negative trigger",
            "submit binance BTCUSDT sell 1 --type stop --trigger -5",
            "--trigger must be a price above zero",
        ),
        (
            "a negative trigger, inline",
            "submit binance BTCUSDT sell 1 --type stop --trigger=-5",
            "--trigger must be a price above zero",
        ),
        (
            "a NaN trigger",
            "submit binance BTCUSDT sell 1 --type stop --trigger nan",
            "--trigger must be a price above zero",
        ),
        (
            "a flag with no value",
            "submit binance BTCUSDT sell 1 --trigger",
            "--trigger needs a value",
        ),
        (
            "a flag that eats a flag",
            "submit binance BTCUSDT sell 1 --type --trigger 9",
            "--type needs a value",
        ),
        ("a book with two slashes", "submit binance/a/b BTCUSDT sell 1", "more than one '/'"),
        ("a lower-case label", "submit binance/alt BTCUSDT sell 1", "binance/alt"),
    ] {
        let err = parse_line(line).expect_err(class);
        assert!(err.starts_with("submit: "), "{class}: {err}");
        assert!(err.contains(needle), "{class}: {err}");
    }
}

/// `--reason` takes the rest of the line, so a trigger flag typed after it would be rationale text and
/// the order would go out as a plain market order. It is refused, in the one-shot verb's words, for
/// both flag spellings and for the buy/sell sugar; a reason that only CONTAINS such words, and any other
/// verb, is left alone.
#[test]
fn a_trigger_flag_after_reason_is_refused_not_swallowed() {
    let refused = |line: &str| {
        let (cmd, reason) = split_reason(line);
        super::repl::grammar_orders::swallowed_flag(cmd, reason.as_deref())
    };
    for line in [
        "submit binance BTCUSDT sell 1 --reason scale out --type stop --trigger 100",
        "submit binance BTCUSDT sell 1 --reason why --trigger=100",
        "sell binance BTCUSDT 1 --reason why --type=stop",
        "SUBMIT binance BTCUSDT sell 1 --reason --type",
    ] {
        let msg = refused(line).unwrap_or_else(|| panic!("{line}: must be refused"));
        assert!(
            msg.starts_with("submit: `--type`") || msg.starts_with("submit: `--trigger`"),
            "{msg}"
        );
        assert!(msg.contains("BEFORE --reason"), "{line}: {msg}");
    }
    for line in [
        "submit binance BTCUSDT sell 1 --type stop --trigger 100 --reason scale out",
        "submit binance BTCUSDT sell 1 --reason the type of trigger is unchanged",
        "submit binance BTCUSDT sell 1",
        "cancel c-1 --reason why --type stop",
        "flatten binance BTCUSDT --reason why --trigger 5",
    ] {
        assert_eq!(refused(line), None, "{line}");
    }
}

/// **A close typed with `--reduce-only` after `--reason` went out as a plain sell, which can OPEN a
/// short**, and a `--order-client-id` there was lost for a minted id. Both are refused like the trigger flags, in
/// both spellings; before `--reason` they are the order's.
#[test]
fn a_reduce_only_or_an_order_client_id_after_reason_is_refused_not_swallowed() {
    let refused = |line: &str| {
        let (cmd, reason) = split_reason(line);
        super::repl::grammar_orders::swallowed_flag(cmd, reason.as_deref())
    };
    for (line, flag) in [
        ("sell binance BTCUSDT 1 --reason close it --reduce-only", "--reduce-only"),
        ("submit binance BTCUSDT buy 1 --reason why --order-client-id abc123", "--order-client-id"),
        ("buy binance BTCUSDT 1 --reason why --order-client-id=abc123", "--order-client-id"),
    ] {
        let msg = refused(line).unwrap_or_else(|| panic!("{line}: must be refused"));
        assert!(msg.contains(&format!("Put `{flag}` BEFORE --reason")), "{line}: {msg}");
    }
    let line = "sell binance BTCUSDT 1 --reduce-only --order-client-id abc123 --reason close it";
    assert_eq!(refused(line), None, "{line}");
    let o = submit(parse_line(split_reason(line).0).unwrap());
    assert!(o.reduce_only);
    assert_eq!(o.order_client_id, "abc123");
}

/// **At this prompt too, `modify C --qty 5 --reason why --price 100` changed only the qty and `mount …
/// --reason why --params {..}` started on default params.** Refused like the one-shot verbs, in both
/// spellings; a flag before `--reason`, a reason that only mentions the word, and a word that is no flag of
/// this line (`--yes` is the session's) are not.
#[test]
fn a_modify_or_mount_flag_after_reason_is_refused_not_swallowed() {
    let refused = |line: &str| {
        let (cmd, reason) = split_reason(line);
        super::repl::grammar_orders::swallowed_flag(cmd, reason.as_deref())
    };
    for (line, verb, flag) in [
        ("modify C --qty 5 --reason why --price 100", "modify", "--price"),
        ("modify C --price 100 --reason why --qty=5", "modify", "--qty"),
        ("mount bybit BTC 1m --strategy grid.rs --reason why --params {}", "mount", "--params"),
        ("mount bybit BTC 1m --strategy grid.rs --reason why --id=m1", "mount", "--id"),
        ("mount bybit BTC 1m --reason why --strategy grid.rs", "mount", "--strategy"),
    ] {
        let msg = refused(line).unwrap_or_else(|| panic!("{line}: must be refused"));
        assert!(msg.starts_with(&format!("{verb}: `{flag}`")), "{line}: {msg}");
        assert!(msg.contains(&format!("Put `{flag}` BEFORE --reason")), "{line}: {msg}");
    }
    for line in [
        "modify C --qty 5 --price 100 --reason why",
        "modify C --qty 5 --reason the price is unchanged",
        "modify C --qty 5 --reason why --yes",
        "mount bybit BTC 1m --strategy grid.rs --params {} --reason why",
        "unmount m1 --reason why --params {}",
    ] {
        assert_eq!(refused(line), None, "{line}");
    }
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
/// parser — the REPL's `parse_line`, the one-shot verb's `parse_submit_args` — and each must refuse
/// with the SAME reason in its own prefix, and the one-shot verb on the usage rung. Before the shared
/// function they disagreed: the two command lines refused only `qty <= 0` / NaN, and the MCP tool
/// refused none of it. The tool's own leg is `crates/vike-mcp/src/tool_verbs_tests.rs`'s
/// `the_mcp_submit_tool_refuses_a_non_positive_qty_at_parse`: JSON has no spelling for NaN or an
/// infinity, so the rows it can carry are those two. All three paths call
/// `crates/vike-client/src/verbs.rs`'s `unsizeable_submit_refusal`.
#[test]
fn the_repl_and_the_one_shot_verb_refuse_the_same_unsizeable_orders() {
    for c in UNSIZEABLE {
        let price_token = c.price.map(|p| format!("@{p}"));

        // 1. the REPL line.
        let mut line = format!("submit binance BTCUSDT buy {}", c.qty);
        if let Some(token) = &price_token {
            line.push(' ');
            line.push_str(token);
        }
        assert_eq!(
            parse_line(&line).unwrap_err(),
            format!("submit: {}", c.reason),
            "REPL, {}: {line}",
            c.class
        );

        // 2. the one-shot verb's argv (book first), on the USAGE rung.
        let mut argv = vec!["binance", "BTCUSDT", "buy", c.qty];
        if let Some(token) = &price_token {
            argv.push(token.as_str());
        }
        let e = super::order::parse_submit_args(&argv).unwrap_err();
        assert_eq!(e.exit, crate::exit::Exit::Usage, "one-shot, {}: {argv:?}", c.class);
        // `-inf` is a single-dash token that is not a negative NUMBER, so the plane's one argv
        // reader refuses it as an unknown flag before the unsizeable rule can name it (decision
        // 0155's single-dash rule) — still refused, on the usage rung, in `clap`'s words.
        if c.qty != "-inf" {
            assert_eq!(
                e.message,
                format!("submit: {}", c.reason),
                "one-shot, {}: {argv:?}",
                c.class
            );
        }
    }
}

/// **The other direction, and the one a parse refusal is judged by.** A sizeable order — over EVERY
/// cap included — still parses on the REPL: the preview line says OVER LIMIT and the order is still
/// built (the write then refuses the notional breach: `vike_client::order_gate::judge`). The parse
/// refusal is for an order with no size, never for a big one.
#[test]
fn a_sizeable_order_over_every_cap_still_parses_and_is_flagged() {
    let capabilities =
        verbs::GuardrailCapabilities { max_qty: Some(1.0), max_notional: Some(10.0) };
    for line in [
        "submit binance BTCUSDT buy 5 @100", // over the qty cap AND the notional cap
        "buy binance BTCUSDT 5",             // a market order: over the qty cap, no price to size
        "sell binance BTCUSDT 5 @market --reduce-only",
        "submit binance BTCUSDT buy 1e3 @1e6", // an enormous but FINITE order
    ] {
        let verb = parse_line(line).unwrap_or_else(|e| panic!("{line:?} must still parse: {e}"));
        let cmd = verb.to_wire_command().expect("a submit is a write");
        let advisory =
            verbs::guardrail_check(&cmd, capabilities, Ok(Default::default()), false).line();
        assert!(advisory.contains("OVER LIMIT"), "{line:?} parses and is flagged: {advisory}");
    }
}

/// A frame with one engine, `binance`, trading BTCUSDT: what the one order gate judges a command-line order against.
fn binance_frame() -> CoreSnapshotWire {
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues.push(
        serde_json::from_value(serde_json::json!({
            "venue": "binance", "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
            "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
            "margin_used": 0.0, "free_buying_power": 0.0, "positions": [],
            "trading_state": "Active", "route_key": "binance", "symbols": ["BTCUSDT"],
        }))
        .expect("a venue block"),
    );
    frame
}

/// **The REPL and the one-shot verb hand the one order gate the same command, and it refuses alike** (the owner's
/// ruling of 2026-11-17, and D1-D5 of 2026-11-24 for both doors): an order over this box's notional cap, a venue the
/// frame shows the node does not run, and a node that cannot route one it cannot see are NOT SENT. At the cap it
/// passes, and an order this box cannot price, a breach of the quantity cap alone and a box with no cap are not
/// refused. The rule is `vike_client::order_gate`'s and its tests are there; this holds the two grammars to one input.
#[test]
fn an_order_over_the_notional_cap_is_refused_by_the_one_gate_on_both_grammars() {
    use vike_client::order_gate::{NodeLink, judge};
    use vike_client::refusals::OrderGateRefusalId as Id;
    let routes = NodeLink { routes_by_venue: Some(true), one_account_door: Some(true) };
    let capped = |max_qty| verbs::GuardrailCapabilities { max_qty, max_notional: Some(100.0) };
    let frame = binance_frame();
    let rows: [(&str, &[&str], verbs::GuardrailCapabilities, Option<Id>); 7] = [
        (
            "submit binance BTCUSDT buy 2 @100",
            &["binance", "BTCUSDT", "buy", "2", "@100"],
            capped(None),
            Some(Id::OverMaxNotional),
        ),
        (
            "submit binance BTCUSDT buy 1 @100",
            &["binance", "BTCUSDT", "buy", "1", "@100"],
            capped(None),
            None,
        ),
        ("buy binance BTCUSDT 5", &["binance", "BTCUSDT", "buy", "5"], capped(None), None),
        (
            "submit binance BTCUSDT buy 2 @10",
            &["binance", "BTCUSDT", "buy", "2", "@10"],
            capped(Some(1.0)),
            None,
        ),
        (
            "submit binance BTCUSDT buy 2 @100",
            &["binance", "BTCUSDT", "buy", "2", "@100"],
            verbs::GuardrailCapabilities::default(),
            None,
        ),
        (
            "submit okx BTC-USDT buy 1 @1",
            &["okx", "BTC-USDT", "buy", "1", "@1"],
            capped(None),
            Some(Id::VenueNotMounted),
        ),
        (
            "submit binance ETHUSDT buy 2 @100",
            &["binance", "ETHUSDT", "buy", "2", "@100"],
            capped(None),
            None,
        ),
    ];
    for (line, argv, capabilities, want) in rows {
        let repl = parse_line(line).unwrap().to_wire_command().unwrap();
        let one_shot = super::order::parse_submit_args(argv).unwrap().to_wire_command().unwrap();
        assert_eq!(repl, one_shot, "the two grammars build one command: {line}");
        let got = judge(&repl, capabilities, Ok(&frame), routes).err();
        assert_eq!(got.as_ref().map(|r| r.id()), want, "{line}");
        if let Some(refusal) = got {
            let printed = crate::cmd::trade::write::gate_refusal_line(&refusal);
            assert!(
                printed.starts_with(&format!("error [{}]: not sent: ", refusal.id().word())),
                "{printed}"
            );
        }
    }
    let over = judge(
        &parse_line(rows[0].0).unwrap().to_wire_command().unwrap(),
        capped(None),
        Ok(&frame),
        routes,
    );
    assert!(over.unwrap_err().to_string().contains("policy.max_notional_per_order 100"));
    // No frame: a node that cannot route refuses a venue it cannot show; one that routes answers for itself.
    let okx = parse_line("submit okx BTC-USDT buy 1 @100").unwrap().to_wire_command().unwrap();
    let old = NodeLink { routes_by_venue: Some(false), one_account_door: Some(true) };
    let held =
        judge(&okx, capped(None), Err("no frame"), old).expect_err("no frame is no evidence");
    assert_eq!(held.id(), Id::NodeDoesNotRoute);
    assert!(held.to_string().contains("published none yet"), "{held}");
    assert!(judge(&okx, capped(None), Err("no frame"), routes).is_ok());
    let cancel = parse_line("cancel c-9").unwrap().to_wire_command().unwrap();
    assert!(judge(&cancel, capped(None), Err("no frame"), old).is_ok(), "no venue named");
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
        Verb::Modify {
            order_client_id: "c-1".into(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        }
    );
    assert_eq!(
        parse_line("modify c-1 --price 100 --qty 3").unwrap(),
        Verb::Modify {
            order_client_id: "c-1".into(),
            new_qty: Some(3.0),
            new_price: Some(100.0),
            new_trigger_price: None,
        }
    );
    assert!(parse_line("modify c-1").is_err()); // nothing to change
    assert!(parse_line("modify").is_err()); // no order client id
    assert!(parse_line("modify c-1 --qty").is_err()); // dangling value
    assert!(parse_line("modify c-1 --bogus 1").is_err()); // unknown flag
}

/// **`modify C --qty 5 --price 1e999` parsed as inf, previewed `price=inf`, and the JSON frame sent `null`,
/// so the node changed only the qty.** A number that is not finite is refused here as at the one-shot
/// (`order_tests.rs`'s `a_modify_number_that_is_not_finite_is_refused`), in the same words; a zero or a
/// negative one stays the node's to judge.
#[test]
fn a_modify_number_that_is_not_finite_is_refused_at_the_prompt() {
    for (flag, raw, what) in [
        ("--price", "1e999", "price"),
        ("--price", "inf", "price"),
        ("--price", "nan", "price"),
        ("--qty", "1e999", "qty"),
        ("--qty", "inf", "qty"),
        ("--qty", "nan", "qty"),
    ] {
        let want = format!("modify: {what} must be a finite number, got {raw:?}");
        assert_eq!(parse_line(&format!("modify C {flag} {raw}")).expect_err(raw), want);
    }
    assert_eq!(
        parse_line("modify C --qty 0 --price -1").unwrap(),
        Verb::Modify {
            order_client_id: "C".into(),
            new_qty: Some(0.0),
            new_price: Some(-1.0),
            new_trigger_price: None,
        }
    );
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
fn parse_close_all() {
    assert_eq!(parse_line("close-all").unwrap(), Verb::MarketExit { venue: None, account: None });
    assert_eq!(
        parse_line("close-all binance").unwrap(),
        Verb::MarketExit { venue: Some("binance".into()), account: None }
    );
    assert!(parse_line("close-all a b").is_err());
}

/// A REPL word that was renamed is refused by its id, naming the word that replaced it, and never
/// read as it: `market-exit`/`panic` are `close-all`, and no shortening stands beside a full word.
#[test]
fn a_renamed_repl_word_is_refused_naming_its_new_word() {
    for (was, now) in super::repl::grammar::REPL_RENAMED {
        for line in [was.to_string(), format!("{was} binance")] {
            let err = parse_repl_line(&line).expect_err("a renamed word is no verb");
            let want = crate::cmd::trade::plane::renamed(*was, *now);
            assert!(want.starts_with("error [cli.trade.renamed]: "), "{want}");
            assert_eq!(err, super::repl::grammar::LineError::Refused(want), "{line}");
        }
        assert!(parse_line(now).is_ok(), "`{now}` is the verb now");
    }
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
    for (line, want) in [("halt", TradingState::Halted), ("resume", TradingState::Active)] {
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
        CommandWire::SetTradingState(TradingState::Halted)
    );
    assert_eq!(
        parse_line("resume").unwrap().to_wire_command().unwrap(),
        CommandWire::SetTradingState(TradingState::Active)
    );
    assert!(
        describe_command(&CommandWire::SetTradingState(TradingState::Halted))
            .contains("SET-STATE halted")
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

    assert!(parse(&["--yes=1"]).unwrap_err().contains("--yes"));
    assert!(parse(&["--wat"]).unwrap_err().contains("--wat"));
    assert!(parse(&["-h"]).unwrap_err().is_help());
    // A bare mode word is not an argument of these verbs either — `halt reducing` is a usage
    // error, not a third state. (A positional is never echoed, so the refusal says `<hidden>`.)
    assert!(!parse(&["reducing"]).unwrap_err().is_help());

    // A repeated `--node` is last-wins, and a `--reason` beginning with a single dash is a value
    // (both shapes sit in runbooks); one beginning with `--` is another flag, refused in the old
    // words, and the `--yes` it would have eaten is not set.
    assert_eq!(parse(&["--node", "a:1", "--node", "b:2"]).unwrap().node.as_deref(), Some("b:2"));
    assert_eq!(parse(&["--reason", "-urgent"]).unwrap().reason.as_deref(), Some("-urgent"));
    assert_eq!(
        parse(&["--reason", "--yes"]).unwrap_err().text(),
        "vike-cli trade halt: --reason requires a value, but the next argument is another flag \
         (--yes)\n"
    );
}

/// The verb NAME is derived from the state it sends, so a message can never say `halt` while
/// sending `Active`, and each help page states what its own verb does.
#[test]
fn the_one_shot_verb_name_follows_the_state_it_sends() {
    assert_eq!(mode_verb(TradingState::Halted), "halt");
    assert_eq!(mode_verb(TradingState::Active), "resume");
    let halt = help_of(&["halt"]);
    assert!(halt.contains("vike-cli trade halt"), "{halt}");
    // The covered-reduce exemption is stated where an operator reads it under pressure: a halt
    // that reads as "I am trapped" gets un-halted, restarting the strategy that caused it.
    assert!(halt.contains("close-all") && halt.contains("flatten"), "{halt}");
    assert!(halt.contains("--node"), "the help lists the flags `clap` reads: {halt}");
    let resume = help_of(&["resume"]);
    assert!(resume.contains("ACTIVE"), "{resume}");
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
        "snapshot",
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

/// The node's Ack of a cancel, mass-cancel or close-all is the receipt for the COMMAND: its line
/// says so and where the refusal of an order the node does not hold is read.
#[test]
fn a_cancel_mass_cancel_or_close_all_ack_says_it_is_not_the_cancel() {
    for line in ["cancel c-9", "mass-cancel sim", "mass-cancel sim BTCUSDT", "close-all"] {
        let cmd = parse_line(line).unwrap().to_wire_command().unwrap();
        let tail = accepted_tail(&cmd, "orders", "positions");
        assert!(tail.contains("not for the cancel"), "{tail}");
        assert!(tail.contains("OrderCancelRejected"), "{tail}");
        assert!(tail.ends_with("; run `orders` to see whether the order is gone"), "{tail}");
    }
}

/// A submit, modify or flatten says the same of its Ack AFTER its unchanged text, so a script that
/// reads the leading words is untouched; every other command keeps its line exactly.
#[test]
fn an_order_write_ack_says_it_is_only_the_receipt_and_where_the_result_is() {
    let unchanged = "run `orders`/`positions` to observe the result; the node's ControlLimits + \
                     RiskGate are the enforcing gate";
    for line in ["submit sim BTCUSDT buy 1 @100", "modify c-9 --qty 5", "flatten sim BTCUSDT"] {
        let cmd = parse_line(line).unwrap().to_wire_command().unwrap();
        let tail = accepted_tail(&cmd, "orders", "positions");
        assert!(tail.starts_with(unchanged), "{line}: the old words stay first: {tail}");
        assert!(tail.contains("only the receipt for the COMMAND"), "{line}: {tail}");
        assert!(tail.contains("recent events") && tail.contains("`recent`"), "{line}: {tail}");
    }
    for line in ["halt", "resume", "mount sim BTCUSDT 1m --strategy grid.rs", "unmount grid"] {
        let cmd = parse_repl_line(line).unwrap().to_wire_command().unwrap();
        assert_eq!(accepted_tail(&cmd, "orders", "positions"), unchanged, "{line}");
    }
    // The one-shot verbs read the same words under their own command names.
    let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
    let tail = accepted_tail(&cmd, "trade order ls", "trade position ls");
    assert!(tail.starts_with("run `trade order ls`/`trade position ls` to observe"), "{tail}");
}

/// Each WRITE verb maps to the correct [`CommandWire`]; each READ verb maps to `None`.
#[test]
fn verb_to_wire_command_mapping() {
    // Submit
    let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
    match cmd {
        CommandWire::Submit(o) => {
            assert_eq!(o.symbol, "BTCUSDT");
            assert_eq!(o.side, 1);
            assert_eq!(o.qty, 1.0);
            assert_eq!(o.price, Some(100.0));
        }
        other => panic!("expected Submit, got {other:?}"),
    }
    assert_eq!(
        parse_line("cancel c-9").unwrap().to_wire_command().unwrap(),
        CommandWire::Cancel("c-9".into())
    );
    assert_eq!(
        parse_line("modify c-9 --qty 5").unwrap().to_wire_command().unwrap(),
        CommandWire::Modify {
            order_client_id: "c-9".into(),
            new_qty: Some(5.0),
            new_price: None,
            new_trigger_price: None,
        }
    );
    assert_eq!(
        parse_line("flatten sim BTCUSDT").unwrap().to_wire_command().unwrap(),
        CommandWire::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None }
    );
    assert_eq!(
        parse_line("mass-cancel sim").unwrap().to_wire_command().unwrap(),
        CommandWire::MassCancel { venue: Some("sim".into()), symbol: None, account: None }
    );
    assert_eq!(
        parse_line("close-all").unwrap().to_wire_command().unwrap(),
        CommandWire::MarketExit { venue: None, account: None }
    );
    assert_eq!(
        parse_line("halt").unwrap().to_wire_command().unwrap(),
        CommandWire::SetTradingState(TradingState::Halted)
    );
}

// -- a bare venue on a session's first line (decision 0254) -----------------------------------------

const FIRST_LINE_KEY: &[u8] = b"DUMMY-observe-key-for-the-first-line-tests";

/// A node that answers the subscribe only after `delay`, with a frame listing two bybit accounts: the session's first
/// line beats it, which is what the CI box measured.
fn node_with_two_bybit_accounts_after(
    delay: std::time::Duration,
) -> (std::net::SocketAddr, std::sync::mpsc::Receiver<Vec<String>>) {
    use vike_tradehub_client::proto::Response;
    use vike_tradehub_client::test_support::fake_node::{features, scripted_node};
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 7;
    for account in ["DEMO", "LIVE"] {
        frame.not_running.push(
            serde_json::from_value(serde_json::json!({
                "venue": "bybit", "account": account, "route_key": format!("bybit#{account}"), "reason": "stopped",
            }))
            .expect("a row"),
        );
    }
    scripted_node(FIRST_LINE_KEY, features(&["observe"]), move |_request| {
        std::thread::sleep(delay);
        ("subscribe".to_string(), Response::SnapshotFrame(Box::new(frame.clone())))
    })
}

/// **The first line of a REPL session got `preview:` and the guardrail's REFUSED line instead of the plain refusal**:
/// the frame in hand was the client's placeholder, which shows no accounts. The write path waits for the node's first
/// frame, and the one order gate refuses on it.
#[test]
fn a_bare_venue_on_the_first_line_waits_for_the_frame_and_gets_the_plain_refusal() {
    let (node, _names) = node_with_two_bybit_accounts_after(std::time::Duration::from_millis(300));
    let observe = TradehubCoreHandle::connect(node, FIRST_LINE_KEY).expect("the real handshake");
    let cmd = parse_line("submit bybit BTCUSDT buy 1 @market").unwrap().to_wire_command().unwrap();
    let frame = crate::cmd::trade::write::write_frame(Some(&observe), &cmd).expect("a frame");
    let link = vike_client::order_gate::NodeLink::default();
    let refusal = vike_client::order_gate::judge(&cmd, Default::default(), Ok(&frame), link)
        .expect_err("two accounts of bybit, once the frame is in");
    let why = crate::cmd::trade::write::gate_refusal_line(&refusal);
    assert!(why.starts_with("error [order.edge.bare_venue_of_several_accounts]: "), "{why}");
    assert!(why.contains("bybit/DEMO") && why.contains("bybit/LIVE"), "{why}");
}

/// Only a command that names a venue or states a size waits for the frame: `halt` and a cancel ask nothing of it.
/// (A named book waits too since 2026-11-24: the gate refuses a venue or an account the frame shows the node does not
/// run.)
#[test]
fn a_command_that_names_no_venue_does_not_wait_for_the_frame() {
    let (node, _names) = node_with_two_bybit_accounts_after(std::time::Duration::from_secs(4));
    let observe = TradehubCoreHandle::connect(node, FIRST_LINE_KEY).expect("the real handshake");
    let started = std::time::Instant::now();
    for line in ["halt", "cancel c-9"] {
        let cmd = parse_line(line).unwrap().to_wire_command().unwrap();
        assert!(crate::cmd::trade::write::write_frame(Some(&observe), &cmd).is_err(), "{line}");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(1), "{:?}", started.elapsed());
}
