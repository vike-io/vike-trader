/// COMPOSED, not spelled. `crates/vike-config/tests/policy_is_consumed.rs` used to read every
/// non-comment line under `src/` for a section-qualified policy key as evidence that the file READS
/// that setting, test modules included, so a fixture spelling one turned `main` red (#1685 lifted
/// these fixtures here from `lib.rs` and `Policy::max_leverage` was `Consumed::No`, as it was then). It now reads the
/// PRODUCTION view only (comments, `#[cfg(test)]` items and test files are skipped), so the
/// composition is no longer required; it is kept because it is harmless.
/// Composing it is the same trick `crates/vike-venues/src/credential_keys.rs` uses to keep its
/// near-miss fixtures out of the settings-registry literal harvest.
const POLICY_KEY_FIXTURE: &str = concat!("policy.", "max_leverage");

use super::*;
use std::assert_matches;
use vike_client::verbs::{
    GuardrailCapabilities, fill_order_client_id, guardrail_check, order_client_id_minter,
};
use vike_model::orders::client_id::OrderClientIdSource;

#[test]
fn modify_tool_args_map_to_the_repl_wire_command() {
    // The SAME CommandWire::Modify the REPL's `modify c-1 --qty 2` builds.
    let cmd =
        wire_command_for("modify", &json!({ "order_client_id": "c-1", "new_qty": 2.0 })).unwrap();
    assert_eq!(
        cmd,
        CommandWire::Modify {
            order_client_id: "c-1".into(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        }
    );
    // `new_trigger_price` alone is a change, and rides the wire as the CLI's `modify --trigger` does.
    let cmd =
        wire_command_for("modify", &json!({ "order_client_id": "c-1", "new_trigger_price": 95.0 }))
            .unwrap();
    assert_eq!(
        cmd,
        CommandWire::Modify {
            order_client_id: "c-1".into(),
            new_qty: None,
            new_price: None,
            new_trigger_price: Some(95.0),
        }
    );
}

/// `display_qty`, `release_at` and `release_when_price` ride the submit onto the wire, and the rules the node's door
/// applies again refuse them first: a display off a limit order or not below the qty, a past release, a price
/// condition with no moment or in no known word.
#[test]
fn the_submit_tool_takes_a_display_and_a_release_by_the_doors_rules() {
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire};
    let at = vike_model::now_ms() + 60_000;
    let base = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 5.0,
                       "order_type": "limit", "price": 100.0 });
    let with = |extra: Value| {
        let mut args = base.clone();
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        wire_command_for("submit_order", &args)
    };
    let Ok(CommandWire::Submit(req)) = with(json!({ "display_qty": 1.0, "release_at": at,
                                                    "release_when_price": "below:90" }))
    else {
        panic!("a displayed, held submit builds");
    };
    assert_eq!(req.display_quantity, Some(1.0));
    assert_eq!(
        req.release,
        Some(OrderReleaseWire {
            at,
            price: Some(OrderReleasePriceWire::Below(90.0)),
            symbol: None,
            venue: None,
        })
    );
    // another symbol's price, named in the word
    let Ok(CommandWire::Submit(req)) =
        with(json!({ "release_at": at, "release_when_price": "above:ETHUSDT:3500" }))
    else {
        panic!("a conditional on another symbol builds");
    };
    assert_eq!(
        req.release,
        Some(OrderReleaseWire {
            at,
            price: Some(OrderReleasePriceWire::Above(3500.0)),
            symbol: Some("ETHUSDT".into()),
            venue: None,
        })
    );
    for (extra, want) in [
        (json!({ "display_qty": 5.0 }), "`display_qty` — the display quantity 5 is not below"),
        (json!({ "release_at": 1_000 }), "`release_at` — the release 1000 is not in the future"),
        (json!({ "release_when_price": "above:1" }), "`release_when_price` needs `release_at`"),
        (
            json!({ "release_at": at, "release_when_price": "over:1" }),
            "above:<price> or below:<price>",
        ),
        (json!({ "release_at": "soon" }), "`release_at` must be an integer"),
    ] {
        let err = with(extra.clone()).expect_err(&extra.to_string());
        assert!(err.contains(want), "{extra}: {err}");
    }
}

/// `trail` and `limit_offset` ride a trailing submit onto the wire (the activation on `trigger_price`), and the rule
/// the node's door applies again refuses a trailing order's bad terms first; a `trail` on another kind is refused.
#[test]
fn the_submit_tool_takes_a_trailing_order_by_the_doors_rules() {
    let with = |extra: Value| {
        let mut args = json!({ "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": -1, "qty": 1.0 });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        wire_command_for("submit_order", &args)
    };
    let Ok(CommandWire::Submit(req)) = with(json!({ "order_type": "trailing_limit", "trail": 50.0,
                                                    "limit_offset": 5.0, "trigger_price": 70000.0 }))
    else {
        panic!("a trailing limit with an activation builds");
    };
    assert_eq!(
        (req.order_type.as_str(), req.trail, req.limit_offset, req.trigger_price),
        ("trailing_limit", Some(50.0), Some(5.0), Some(70000.0))
    );
    for (extra, want) in [
        (json!({ "order_type": "trailing" }), "needs a trail distance above zero"),
        (json!({ "order_type": "trailing", "trail": 5.0, "price": 100.0 }), "takes no price"),
        (
            json!({ "order_type": "trailing_limit", "trail": 5.0 }),
            "needs a limit offset of zero or more",
        ),
        (
            json!({ "order_type": "trailing", "trail": 5.0, "limit_offset": 1.0 }),
            "belongs to a trailing_limit order",
        ),
        (
            json!({ "order_type": "limit", "price": 100.0, "trail": 5.0 }),
            "`trail` belongs to a trailing or trailing_limit order",
        ),
        (json!({ "order_type": "trailing", "trail": "far" }), "`trail` must be a number"),
    ] {
        let err = with(extra.clone()).expect_err(&extra.to_string());
        assert!(err.starts_with("submit_order: "), "{extra}: {err}");
        assert!(err.contains(want), "{extra}: {err}");
    }
}

/// `peg` and `peg_offset` ride a pegged submit onto the wire (no offset is 0), and the door's rule refuses a pegged
/// order's bad terms first: no peg, a price, an anchor that is no word of ours, a peg on another kind.
#[test]
fn the_submit_tool_takes_a_pegged_order_by_the_doors_rules() {
    use vike_tradehub_client::wire::OrderPegWire;
    let with = |extra: Value| {
        let mut args =
            json!({ "venue": "ibkr", "symbol": "AAPL.SMART.USD", "side": 1, "qty": 1.0 });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        wire_command_for("submit_order", &args)
    };
    let Ok(CommandWire::Submit(req)) =
        with(json!({ "order_type": "pegged", "peg": "mid", "peg_offset": -0.05 }))
    else {
        panic!("a pegged order builds");
    };
    assert_eq!(req.order_type, "pegged");
    assert_eq!(req.peg, Some(OrderPegWire { anchor: "mid".into(), offset: -0.05 }));
    let Ok(CommandWire::Submit(req)) = with(json!({ "order_type": "pegged", "peg": "best" }))
    else {
        panic!("a pegged order with no offset builds");
    };
    assert_eq!(req.peg, Some(OrderPegWire { anchor: "best".into(), offset: 0.0 }));
    for (extra, want) in [
        (json!({ "order_type": "pegged" }), "a pegged order needs its peg"),
        (json!({ "order_type": "pegged", "peg": "mid", "price": 100.0 }), "takes no price"),
        (
            json!({ "order_type": "pegged", "peg": "Mid" }),
            "`peg` — the anchor \"Mid\" is not one of",
        ),
        (json!({ "order_type": "pegged", "peg_offset": 1.0 }), "`peg_offset` needs `peg`"),
        (
            json!({ "order_type": "limit", "price": 100.0, "peg": "mid" }),
            "a peg belongs to a pegged order",
        ),
    ] {
        let err = with(extra.clone()).expect_err(&extra.to_string());
        assert!(err.starts_with("submit_order: "), "{extra}: {err}");
        assert!(err.contains(want), "{extra}: {err}");
    }
}

/// `submit_order`'s arguments over `base`, `extra`'s keys laid on top, built through the tool's one entry.
fn submit_with(base: &Value, extra: Value) -> Result<CommandWire, String> {
    let mut args = base.clone();
    args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    wire_command_for("submit_order", &args)
}

/// Each refusal of `rows` is the tool's own (`submit_order: `) and says what the row expects.
fn assert_refused(base: &Value, rows: Vec<(Value, &str)>) {
    for (extra, want) in rows {
        let err = submit_with(base, extra.clone()).expect_err(&extra.to_string());
        assert!(err.starts_with("submit_order: "), "{extra}: {err}");
        assert!(err.contains(want), "{extra}: {err}");
    }
}

/// `all_or_none`, `sweep_to_fill` and `discretionary_amount` ride a limit onto the wire as its attributes (`None`
/// unless one is set, an explicit `false` included), a display of 0 (hidden) builds, and the door's rule refuses
/// an attribute off a limit, all-or-none at once, a sweep that may not take, and a discretion that is no amount.
#[test]
fn the_submit_tool_takes_the_attributes_by_the_doors_rule() {
    let base = json!({ "venue": "ibkr", "symbol": "AAPL.SMART.USD", "side": 1, "qty": 10.0,
                       "order_type": "limit", "price": 100.0 });
    let attributes = |extra: Value| match submit_with(&base, extra) {
        Ok(CommandWire::Submit(req)) => req.attributes,
        other => panic!("a limit builds: {other:?}"),
    };
    assert_eq!(
        attributes(json!({ "all_or_none": true, "sweep_to_fill": true,
                           "discretionary_amount": 0.05 })),
        Some(OrderAttributes {
            all_or_none: true,
            sweep_to_fill: true,
            discretionary_amount: Some(0.05),
        })
    );
    assert_eq!(attributes(json!({})), None, "no attribute, no key");
    assert_eq!(attributes(json!({ "all_or_none": false })), None, "never an empty set");
    let Ok(CommandWire::Submit(hidden)) = submit_with(&base, json!({ "display_qty": 0.0 })) else {
        panic!("a hidden limit builds");
    };
    assert_eq!(hidden.display_quantity, Some(0.0));
    assert_refused(
        &base,
        vec![
            (
                json!({ "order_type": "market", "price": null, "all_or_none": true }),
                "an order attribute (all-or-none, sweep-to-fill, discretion) belongs to a limit order",
            ),
            (
                json!({ "all_or_none": true, "time_in_force": "ioc" }),
                "all-or-none at once is `fok`",
            ),
            (
                json!({ "sweep_to_fill": true, "time_in_force": "post_only" }),
                "a sweep-to-fill order takes liquidity at once",
            ),
            (json!({ "discretionary_amount": 0.0 }), "the discretionary amount 0 is not"),
            (json!({ "all_or_none": "yes" }), "`all_or_none` must be true or false"),
        ],
    );
}

/// `trail_percent` rides a trailing order onto the wire in place of `trail`, and `trailing_take_profit` builds by
/// either distance and an optional `limit_offset`; the door's rule refuses both distances, a percent out of range or
/// off a trailing kind, and a trailing take-profit's price or activation.
#[test]
fn the_submit_tool_takes_a_percent_trail_and_a_trailing_take_profit() {
    let base = json!({ "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": -1, "qty": 1.0 });
    let built = |extra: Value| match submit_with(&base, extra) {
        Ok(CommandWire::Submit(req)) => {
            (req.order_type, req.trail, req.trail_percent, req.limit_offset)
        }
        other => panic!("a trailing order builds: {other:?}"),
    };
    assert_eq!(
        built(json!({ "order_type": "trailing", "trail_percent": 1.5 })),
        ("trailing".to_string(), None, Some(1.5), None)
    );
    assert_eq!(
        built(json!({ "order_type": "trailing_take_profit", "trail": 50.0, "limit_offset": 5.0 })),
        ("trailing_take_profit".to_string(), Some(50.0), None, Some(5.0))
    );
    assert_eq!(
        built(json!({ "order_type": "trailing_take_profit", "trail_percent": 2.0 })),
        ("trailing_take_profit".to_string(), None, Some(2.0), None)
    );
    assert_refused(
        &base,
        vec![
            (
                json!({ "order_type": "trailing", "trail": 5.0, "trail_percent": 1.0 }),
                "trails by a distance or by a percent, not both",
            ),
            (
                json!({ "order_type": "trailing", "trail_percent": 100.0 }),
                "the trail percent 100 of a trailing order is not above 0 and below 100",
            ),
            (
                json!({ "order_type": "limit", "price": 100.0, "trail_percent": 1.0 }),
                "a trail percent belongs to a trailing order",
            ),
            (
                json!({ "order_type": "trailing_take_profit", "trail": 5.0, "trigger_price": 9.0 }),
                "takes no activation price",
            ),
            (
                json!({ "order_type": "trailing_take_profit", "trail": 5.0, "price": 100.0 }),
                "takes no price",
            ),
            (json!({ "order_type": "trailing_take_profit" }), "needs a trail distance above zero"),
        ],
    );
}

/// `adjust_at` and its target ride a stop onto the wire as its adjustment, each target in its own terms, through
/// the reader the CLI shares; a target's term with no `adjust_at`, a level with no `adjust_to`, a term the target
/// does not take and the door's rule (the kind, the protected side, the near side) are each refused in words.
#[test]
fn the_submit_tool_takes_a_stop_adjustment_by_the_doors_rule() {
    use vike_model::OrderStopAdjustTo;
    // A sell stop at 90 under a long; at 110 it becomes its target.
    let base = json!({ "venue": "ibkr", "symbol": "AAPL.SMART.USD", "side": -1, "qty": 10.0,
                       "order_type": "stop", "trigger_price": 90.0, "adjust_at": 110.0 });
    let adjust = |extra: Value| match submit_with(&base, extra) {
        Ok(CommandWire::Submit(req)) => req.adjust,
        other => panic!("an adjustable stop builds: {other:?}"),
    };
    let at = |to| Some(OrderStopAdjust { at: 110.0, to });
    assert_eq!(
        adjust(json!({ "adjust_to": "stop", "adjust_stop": 100.0 })),
        at(OrderStopAdjustTo::Stop { stop: 100.0 })
    );
    assert_eq!(
        adjust(json!({ "adjust_to": "stop_limit", "adjust_stop": 100.0, "adjust_limit": 99.0 })),
        at(OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 })
    );
    assert_eq!(
        adjust(
            json!({ "adjust_to": "trailing", "adjust_stop": 100.0, "adjust_trail_percent": 2.0 })
        ),
        at(OrderStopAdjustTo::Trailing { stop: 100.0, trail: None, trail_percent: Some(2.0) })
    );
    assert_eq!(adjust(json!({ "adjust_at": null })), None, "no adjustment, no key");
    assert_refused(
        &base,
        vec![
            (json!({ "adjust_at": null, "adjust_stop": 100.0 }), "`adjust_stop` needs `adjust_at`"),
            (json!({ "adjust_stop": 100.0 }), "`adjust_at` needs `adjust_to`"),
            (
                json!({ "adjust_to": "stop", "adjust_stop": 100.0, "adjust_limit": 99.0 }),
                "takes no limit price",
            ),
            (
                json!({ "adjust_to": "bracket", "adjust_stop": 100.0 }),
                "is not an adjustment target",
            ),
            (json!({ "adjust_to": "stop" }), "needs its target's stop price"),
            (
                json!({ "order_type": "limit", "price": 100.0, "trigger_price": null,
                        "adjust_to": "stop", "adjust_stop": 100.0 }),
                "an adjustment belongs to a stop or a stop_limit order",
            ),
            (
                json!({ "adjust_at": 80.0, "adjust_to": "stop", "adjust_stop": 70.0 }),
                "not on the protected side of the stop",
            ),
            (
                json!({ "adjust_to": "stop", "adjust_stop": 120.0 }),
                "not on the near side of its level",
            ),
        ],
    );
}

/// `release_when_price_on` rides a held submit's price condition onto the wire as the release's venue, as typed (the
/// node writes its own venue as none); it needs a condition, the condition must name its symbol, and the venue must
/// be a roster id, each refused before anything is sent.
#[test]
fn the_submit_tool_reads_a_price_condition_on_another_venue() {
    use vike_tradehub_client::wire::OrderReleasePriceWire;
    let at = vike_model::now_ms() + 60_000;
    let base = json!({ "venue": "ibkr", "symbol": "AAPL.SMART.USD", "side": 1, "qty": 1.0,
                       "order_type": "limit", "price": 100.0, "release_at": at,
                       "release_when_price": "above:BTCUSDT:100000" });
    let Ok(CommandWire::Submit(req)) =
        submit_with(&base, json!({ "release_when_price_on": "binance" }))
    else {
        panic!("a release priced on another venue builds");
    };
    assert_eq!(
        req.release,
        Some(OrderReleaseWire {
            at,
            price: Some(OrderReleasePriceWire::Above(100_000.0)),
            symbol: Some("BTCUSDT".into()),
            venue: Some("binance".into()),
        })
    );
    let Ok(CommandWire::Submit(own)) = submit_with(&base, json!({})) else {
        panic!("a release on the order's own venue builds");
    };
    assert_eq!(own.release.and_then(|r| r.venue), None, "no venue named, no key");
    assert_refused(
        &base,
        vec![
            (
                json!({ "release_when_price": null, "release_when_price_on": "binance" }),
                "`release_when_price_on` needs `release_when_price`",
            ),
            (
                json!({ "release_when_price_on": "Binance" }),
                "`release_when_price_on` — the price venue `Binance` is not a venue",
            ),
            (
                json!({ "release_when_price": "above:100000", "release_when_price_on": "binance" }),
                "names its symbol",
            ),
            (json!({ "release_when_price_on": 5 }), "`release_when_price_on` must be text"),
        ],
    );
}

/// `time_in_force` takes every lifetime word in the node's own reader (`at_open` or `at-open`, any case), goes out
/// in the wire's spelling, is ABSENT for the default `gtc`, and is judged by the door's rule first: a lifetime off a
/// limit order (an auction on a market one passes), `gtd` with no or a past `gtd_expiry`. A `market_to_limit` goes
/// out by its word, and with a trigger is refused.
#[test]
fn the_submit_tool_takes_a_lifetime_and_a_market_to_limit() {
    let base = |order_type: &str, price: Option<f64>| {
        let mut args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0,
                               "order_type": order_type });
        if let Some(px) = price {
            args["price"] = json!(px);
        }
        args
    };
    let with = |mut args: Value, extra: Value| {
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        wire_command_for("submit_order", &args)
    };
    let lifetime = |cmd: Result<CommandWire, String>| match cmd {
        Ok(CommandWire::Submit(req)) => (req.time_in_force, req.gtd_expiry),
        other => panic!("a submit builds: {other:?}"),
    };
    let expiry = vike_model::now_ms() + 60_000;
    for (order_type, price, word, want) in [
        ("market", None, "at_open", "at-open"),
        ("market", None, "AT-CLOSE", "at-close"),
        ("limit", Some(100.0), "at_close", "at-close"),
        ("limit", Some(100.0), "post_only", "post-only"),
        ("limit", Some(100.0), "ioc", "ioc"),
    ] {
        let got = lifetime(with(base(order_type, price), json!({ "time_in_force": word })));
        assert_eq!(got, (Some(want.to_string()), None), "{order_type} {word}");
    }
    let gtd =
        with(base("limit", Some(100.0)), json!({ "time_in_force": "gtd", "gtd_expiry": expiry }));
    assert_eq!(lifetime(gtd), (Some("gtd".to_string()), Some(expiry)));
    let gtc = with(base("limit", Some(100.0)), json!({ "time_in_force": "gtc" }));
    assert_eq!(lifetime(gtc), (None, None), "the default is the absent field");
    for (args, want) in [
        (with(base("market", None), json!({ "time_in_force": "day" })), "belongs to a limit order"),
        (with(base("limit", Some(1.0)), json!({ "time_in_force": "gtd" })), "needs an expiry"),
        (
            with(base("limit", Some(1.0)), json!({ "time_in_force": "gtd", "gtd_expiry": 1_000 })),
            "is not in the future",
        ),
        (
            with(base("limit", Some(1.0)), json!({ "time_in_force": "forever" })),
            "unknown time in force",
        ),
        (
            with(base("market_to_limit", None), json!({ "trigger_price": 9.0 })),
            "a market_to_limit order takes no trigger price",
        ),
    ] {
        let err = args.expect_err(want);
        assert!(err.contains(want), "{want}: {err}");
        assert!(err.starts_with("submit_order: "), "{err}");
    }
    let Ok(CommandWire::Submit(mtl)) = with(base("market_to_limit", None), json!({})) else {
        panic!("a market_to_limit builds");
    };
    assert_eq!((mtl.order_type.as_str(), mtl.price), ("market_to_limit", None));
    // The opening auction's market-to-limit (IB `AUC`) passes; the closing one leaves no session for a remainder.
    let auction = with(base("market_to_limit", None), json!({ "time_in_force": "at_open" }));
    assert_eq!(lifetime(auction), (Some("at-open".to_string()), None));
    let err = with(base("market_to_limit", None), json!({ "time_in_force": "at_close" }))
        .expect_err("at_close on a market_to_limit");
    assert!(err.contains("belongs to a market or a limit order"), "{err}");
}

#[test]
fn modify_with_nothing_to_change_is_a_clean_error() {
    let err = wire_command_for("modify", &json!({ "order_client_id": "c-1" })).unwrap_err();
    assert!(err.contains("new_qty"), "{err}");
    assert!(wire_command_for("modify", &json!({})).unwrap_err().contains("order_client_id"));
}

#[test]
fn mass_cancel_tool_args_map_with_both_scopes_optional() {
    assert_eq!(
        wire_command_for("mass_cancel", &json!({})).unwrap(),
        CommandWire::MassCancel { venue: None, symbol: None, account: None }
    );
    assert_eq!(
        wire_command_for("mass_cancel", &json!({ "venue": "sim", "symbol": "BTCUSDT" })).unwrap(),
        CommandWire::MassCancel {
            venue: Some("sim".into()),
            symbol: Some("BTCUSDT".into()),
            account: None
        }
    );
}

/// The MCP write roster with one MINIMAL argument set each — the seven order verbs and the
/// three node-lifecycle ones. Spelled ONCE so the two roster loops below cannot come to cover
/// different sets; `mcp`'s own `WRITE_TOOLS` is the authority for the names, and
/// `the_write_arm_and_the_write_roster_are_the_same_set` there is what holds the routing to it.
fn write_tool_arguments() -> Vec<(&'static str, Value)> {
    vec![
        ("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
        (
            "submit_order_list",
            json!({ "venue": "sim", "symbol": "B", "side": -1, "qty": 1.0, "shape": "oco",
                    "limit_price": 110.0, "stop_price": 90.0, "reduce_only": true }),
        ),
        (
            "submit_combo",
            json!({ "venue": "sim", "side": 1, "qty": 1.0, "net_limit": 0.5,
                    "legs": [{ "symbol": "C1", "ratio": 1 }, { "symbol": "C2", "ratio": -1 }] }),
        ),
        ("cancel_order", json!({ "order_client_id": "c-1" })),
        ("modify", json!({ "order_client_id": "c-1", "new_price": 9.0 })),
        ("flatten", json!({ "venue": "sim", "symbol": "B" })),
        ("market_exit", json!({})),
        ("set_trading_state", json!({ "state": "halted" })),
        ("mass_cancel", json!({})),
        (
            "mount_strategy",
            json!({ "venue": "sim", "symbol": "B", "interval": "1m", "name": "spread_maker" }),
        ),
        ("unmount_strategy", json!({ "controller_id": "sim__B__1m" })),
        // A key and a value, and nothing else (`docs/decisions/0086`): any section builds the same
        // way, and there is no retype gate left for a policy key to meet, on either surface.
        ("set_setting", json!({ "key": "config.tradehub_addr", "value": "1:2" })),
    ]
}

#[test]
fn every_mcp_write_tool_resolves_through_the_shared_verb() {
    // The whole MCP write roster maps through Verb::to_wire_command — the one construction
    // site. (submit/cancel/flatten/market_exit/set_trading_state pins live in lib.rs's tests;
    // this pins that the mapping goes THROUGH a write Verb for each roster name.)
    for (name, args) in write_tool_arguments() {
        let verb = verb_from_tool_args(name, &args).unwrap();
        assert!(verb.is_write(), "{name} must map to a WRITE verb");
        assert!(verb.to_wire_command().is_some(), "{name} must build a wire command");
    }
    assert!(wire_command_for("node_snapshot", &json!({})).unwrap_err().contains("no wire"));
}

/// The rationale rides BESIDE the command, never inside it: adding a `reason` argument must
/// leave `wire_command_for`'s output BYTE-IDENTICAL for every write tool, and the reason itself
/// comes out of the separate [`reason_from_tool_args`] reader.
#[test]
fn a_reason_argument_never_changes_the_wire_command() {
    for (name, mut args) in write_tool_arguments() {
        let bare = wire_command_for(name, &args).unwrap();
        assert_eq!(reason_from_tool_args(&args), None, "{name}: no reason argument given");
        args["reason"] = json!("agent: flattening ahead of the CPI print");
        let with = wire_command_for(name, &args).unwrap();
        assert_eq!(with, bare, "{name}: a reason must not change the built command");
        assert_eq!(
            reason_from_tool_args(&args).as_deref(),
            Some("agent: flattening ahead of the CPI print"),
            "{name}: …and it is read off the SEPARATE reason reader"
        );
    }
}

#[test]
fn reason_from_tool_args_trims_and_treats_blank_as_absent() {
    assert_eq!(reason_from_tool_args(&json!({ "reason": "  why  " })).as_deref(), Some("why"));
    assert_eq!(reason_from_tool_args(&json!({ "reason": "   " })), None);
    assert_eq!(reason_from_tool_args(&json!({ "reason": "" })), None);
    assert_eq!(reason_from_tool_args(&json!({})), None);
    // A non-string `reason` is ignored rather than stringified into a nonsense rationale.
    assert_eq!(reason_from_tool_args(&json!({ "reason": 7 })), None);
}

// ---- the node-LIFECYCLE three (moved here with the arms they exercise) --------------------
//
// These pin the PURE builder and moved out of `lib.rs` with it; the gates that sit in FRONT of
// it there (the mandatory preview, the venue check) keep their tests in that file, because they
// are the MCP surface's own and not part of the construction.

/// `mount_strategy` takes EXACTLY ONE source, and both wrong shapes are refused HERE rather
/// than at the daemon's edge — a round trip to learn a rule the profile vocabulary already
/// states. (The REPL refuses the same rule in ITS spelling, naming `--name`/`--rhai`; that
/// half is `crates/vike-cli/src/cmd/trade.rs`'s, deliberately.)
#[test]
fn mount_strategy_takes_exactly_one_strategy_source() {
    let base = json!({ "venue": "sim", "symbol": "BTCUSDT", "interval": "1m" });
    let mut neither = base.clone();
    neither["params"] = json!({});
    let err = wire_command_for("mount_strategy", &neither).unwrap_err();
    assert!(err.contains("exactly one"), "{err}");
    assert!(err.contains("not script source"), "…and says what `rhai` is NOT: {err}");

    let mut both = base.clone();
    both["name"] = json!("spread_maker");
    both["rhai"] = json!("strategies/breaker.rhai");
    assert!(wire_command_for("mount_strategy", &both).unwrap_err().contains("both were given"));

    for (field, value) in [("name", "spread_maker"), ("rhai", "strategies/breaker.rhai")] {
        let mut one = base.clone();
        one[field] = json!(value);
        assert!(
            wire_command_for("mount_strategy", &one).is_ok(),
            "{field} alone must build a mount"
        );
    }
}

/// The mount's `params` table: absent means an EMPTY table, and a non-object is refused here
/// because the wire carries it opaquely and nothing below would object until the node did.
#[test]
fn mount_strategy_params_default_to_an_empty_table_and_must_be_an_object() {
    let base = json!({
        "venue": "sim", "symbol": "BTCUSDT", "interval": "1m", "name": "spread_maker"
    });
    let cmd = wire_command_for("mount_strategy", &base).unwrap();
    let CommandWire::MountStrategy { params, controller_id, .. } = &cmd else {
        panic!("the builder produced a different variant")
    };
    assert_eq!(*params, json!({}), "an absent params table is an empty one");
    assert_eq!(*controller_id, None, "…and an absent id derives on the node, not here");

    let mut list = base.clone();
    list["params"] = json!([1, 2]);
    assert!(wire_command_for("mount_strategy", &list).unwrap_err().contains("must be an object"));
}

/// `unmount_strategy` names the mount by id and nothing else — and the refusal says WHICH id.
#[test]
fn unmount_strategy_requires_the_mount_id() {
    let err = wire_command_for("unmount_strategy", &json!({})).unwrap_err();
    assert!(err.contains("controller_id"), "{err}");
    assert_eq!(
        wire_command_for("unmount_strategy", &json!({ "controller_id": "sim__B__1m" })).unwrap(),
        CommandWire::UnmountStrategy { controller_id: "sim__B__1m".to_string() }
    );
}

/// `set_setting`'s `value` is TEXT on the wire. A JSON number or boolean is RENDERED rather
/// than refused (the agent expressed the right intent and the node parses TOML anyway); a
/// structured value is refused, because `serde_json`'s rendering of one would not be TOML.
#[test]
fn a_settings_value_renders_a_scalar_and_refuses_a_structure() {
    let of = |v: Value| {
        wire_command_for("set_setting", &json!({ "key": "config.tradehub_addr", "value": v }))
    };
    for (given, expect) in [(json!(250), "250"), (json!(true), "true"), (json!("x"), "x")] {
        let cmd = of(given.clone()).unwrap();
        let CommandWire::SetSetting { value, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(value, expect, "{given} must render as {expect}");
    }
    assert!(of(json!({ "a": 1 })).unwrap_err().contains("must be text"));
    assert!(of(json!([1])).unwrap_err().contains("must be text"));
}

/// **A settings write's wire `file` is DERIVED from its key's first segment** — step 1 of taking the
/// file era out of the settings wire. A write is one row named by its KEY; the node reads neither
/// `file` nor `confirm`, and the released v0.1.35 daemon still requires `file` on decode, so every
/// client here keeps sending one — the key's own section word, never an input a caller could make
/// disagree with the key. `set_setting` takes no `file` argument, and one an agent still passes is
/// ignored like any argument a tool does not name.
#[test]
fn a_settings_write_derives_its_file_from_the_key() {
    for (args, section) in [
        (json!({ "key": POLICY_KEY_FIXTURE, "value": "3" }), "policy"),
        (json!({ "key": "config.tradehub_addr", "value": "1:2" }), "config"),
        (json!({ "key": "preferences.log_level", "value": "debug" }), "preferences"),
        // A stale `file` that disagrees with the key is not what goes out: the KEY decides.
        (json!({ "file": "config.toml", "key": POLICY_KEY_FIXTURE, "value": "3" }), "policy"),
    ] {
        let cmd = wire_command_for("set_setting", &args).expect("a key and a value are enough");
        let CommandWire::SetSetting { file, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(file, section, "{args}: the wire's file is the key's section word");
    }
}

/// **Every settings write this crate builds goes out with `confirm: None`.** The node has ignored
/// the wire's field since `docs/decisions/0086` point 7 (no retype confirm, for any key), and the
/// field stays on the wire only for an older client — so the ONE construction site both surfaces
/// resolve through fills it with nothing, whatever it is handed: not from `key`, and not from a
/// `policy_confirm` an agent still passes out of an old prompt, which is now an argument nothing
/// reads (every tool ignores what it does not name).
#[test]
fn every_settings_write_is_built_with_no_confirm() {
    for args in [
        json!({ "key": POLICY_KEY_FIXTURE, "value": "3" }),
        json!({ "key": "config.tradehub_addr", "value": "1:2" }),
        json!({ "key": POLICY_KEY_FIXTURE, "value": "3", "policy_confirm": POLICY_KEY_FIXTURE }),
    ] {
        let cmd = wire_command_for("set_setting", &args).expect("a settings write builds");
        let CommandWire::SetSetting { confirm, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(*confirm, None, "{args}: the wire's confirm field must go out empty");
    }
}

/// The MCP write path builds through `wire_command_for`, which still emits an EMPTY order client id when
/// the agent omits the optional argument — so the mint is what makes that path work too, and an
/// agent-supplied id still wins.
#[test]
fn the_mcp_submit_tool_gets_a_minted_order_client_id_when_the_agent_omits_one() {
    let mut minter = order_client_id_minter(OrderClientIdSource::Mcp);
    let bare = wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let CommandWire::Submit(o) = fill_order_client_id(bare, &mut minter, None, |_| false) else {
        panic!("still a Submit")
    };
    assert!(vike_model::is_valid_crypto_order_client_id(&o.order_client_id));
    assert!(o.order_client_id.starts_with('M'), "the MCP's letter: {}", o.order_client_id);

    let pinned = wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0, "order_client_id": "agentPinned1" }),
        )
        .unwrap();
    let CommandWire::Submit(o) = fill_order_client_id(pinned, &mut minter, None, |_| false) else {
        panic!("still a Submit")
    };
    assert_eq!(o.order_client_id, "agentPinned1", "an agent-supplied id is authoritative");
}

// -- the unsizeable-order refusal: the MCP tool's use of it --------------------------------------
//
// The REPL and the one-shot verb agreeing on the SAME classes is one table driving both real parsers,
// `crates/vike-cli/src/cmd/trade/tests/submit.rs`'s `UNSIZEABLE`. What is said HERE: the tool's refusal
// of the rows JSON can carry, and its own acceptance of every sizeable shape. The shared function's
// own rows are `vike_client::verbs`' tests.

/// `submit_order`'s arguments: a MARKET order for one unit, with `extra`'s keys laid over it.
fn submit_args(extra: Value) -> Value {
    let mut args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    for (k, v) in extra.as_object().expect("`extra` is an object") {
        args[k.as_str()] = v.clone();
    }
    args
}

/// The tool's own rows for the two classes JSON CAN carry: a `qty` of zero or below.
#[test]
fn the_mcp_submit_tool_refuses_a_non_positive_qty_at_parse() {
    for (class, qty) in [("zero", 0.0), ("negative", -1.0), ("negative zero", -0.0)] {
        let args = submit_args(json!({ "qty": qty }));
        assert_eq!(
            verb_from_tool_args("submit_order", &args).unwrap_err(),
            "submit_order: qty must be > 0",
            "{class}"
        );
        // …and the one entry the tool actually calls refuses it the same way.
        assert!(wire_command_for("submit_order", &args).is_err(), "{class}");
    }
}

/// A `limit` with no `price` — the one class only this path can build, since the two command-line
/// grammars make a limit BY naming its `@price`. An explicit `null`, a price smuggled in as the
/// trigger, and a different casing of the type are all the same absence.
#[test]
fn the_mcp_submit_tool_refuses_a_limit_order_with_no_price() {
    for (class, extra) in [
        ("order_type only", json!({ "order_type": "limit" })),
        ("price null", json!({ "order_type": "limit", "price": null })),
        ("trigger_price is not a price", json!({ "order_type": "limit", "trigger_price": 9.0 })),
        ("another casing", json!({ "order_type": "LIMIT" })),
    ] {
        let args = submit_args(extra);
        assert_eq!(
            verb_from_tool_args("submit_order", &args).unwrap_err(),
            "submit_order: a limit order needs a price",
            "{class}"
        );
    }
}

/// JSON has no NaN and no infinity, and `json!` of one is `null` — so on THIS path a non-finite
/// `qty` is a MISSING field, which the tool already refused. A `qty` in text is no number either,
/// however it would parse. Pinned so the shape cannot start reading one as a number.
#[test]
fn a_non_finite_qty_is_refused_as_missing_and_a_text_one_as_text() {
    for (class, qty) in
        [("NaN becomes null", json!(f64::NAN)), ("+inf becomes null", json!(f64::INFINITY))]
    {
        let err =
            verb_from_tool_args("submit_order", &submit_args(json!({ "qty": qty }))).unwrap_err();
        assert!(err.contains("requires `qty`"), "{class}: {err}");
    }
    for text in ["inf", "5"] {
        let err =
            verb_from_tool_args("submit_order", &submit_args(json!({ "qty": text }))).unwrap_err();
        assert_eq!(err, format!("submit_order: `qty` must be a number - got \"{text}\""));
    }
}

/// Every sizeable shape the tool took before still builds — and a sizeable order over every cap is
/// still built and still only ADVISED about.
#[test]
fn the_mcp_submit_tool_still_builds_every_sizeable_shape_and_the_guardrail_only_advises() {
    for (class, extra) in [
        ("default market", json!({})),
        ("limit with a price", json!({ "order_type": "limit", "price": 100.0 })),
        ("stop, trigger only", json!({ "order_type": "stop", "trigger_price": 90.0 })),
        (
            "stop with a price",
            json!({ "order_type": "stop", "trigger_price": 90.0, "price": 100.0 }),
        ),
        (
            "take_profit, trigger only",
            json!({ "order_type": "take_profit", "trigger_price": 110.0 }),
        ),
        ("reduce-only market", json!({ "reduce_only": true })),
        ("limit at price zero", json!({ "order_type": "limit", "price": 0.0 })),
        ("limit at a negative price", json!({ "order_type": "limit", "price": -5.0 })),
    ] {
        let args = submit_args(extra);
        let verb = verb_from_tool_args("submit_order", &args)
            .unwrap_or_else(|e| panic!("{class} must still build: {e}"));
        assert_matches!(verb, Verb::Submit(_), "{class}");
    }

    let caps = GuardrailCapabilities { max_qty: Some(1.0), max_notional: Some(10.0) };
    for (class, extra) in [
        ("limit over both caps", json!({ "qty": 500.0, "order_type": "limit", "price": 100.0 })),
        ("market over the qty cap", json!({ "qty": 500.0 })),
    ] {
        let cmd = wire_command_for("submit_order", &submit_args(extra))
            .unwrap_or_else(|e| panic!("{class} must still build: {e}"));
        let g = guardrail_check(&cmd, caps, Ok(Default::default()), false);
        assert_eq!(g.to_json()["within_limits"], false, "{class}");
        assert!(g.line().contains("OVER LIMIT"), "{class}: {}", g.line());
    }
}

/// **A market order that states a price is refused, spelled or by default.** The guardrail sizes it
/// at that price while the venue fills at the market, so `{qty: 10, price: 1}` with no `order_type`
/// previewed a notional of 10 and `within_limits: true`.
#[test]
fn the_mcp_submit_tool_refuses_a_market_order_that_states_a_price() {
    for extra in [
        json!({ "qty": 10.0, "price": 1.0 }),
        json!({ "qty": 10.0, "price": 1.0, "order_type": "market" }),
        json!({ "qty": 10.0, "price": 1.0, "order_type": "MARKET" }),
    ] {
        let err = verb_from_tool_args("submit_order", &submit_args(extra.clone())).unwrap_err();
        assert!(err.starts_with("submit_order: a market order takes no price"), "{extra}: {err}");
        assert!(err.contains("drop the price, or use a limit order"), "{extra}: {err}");
    }
}

/// **A market order that states a trigger is refused the same way, spelled or by default.** The node
/// sizes it at the trigger while Binance ignores the trigger and fills at the market, so
/// `{qty: 10, trigger_price: 1}` walked past a notional ceiling. A real stop keeps its trigger:
/// `the_mcp_submit_tool_still_builds_every_sizeable_shape_and_the_guardrail_only_advises`.
#[test]
fn the_mcp_submit_tool_refuses_a_market_order_that_states_a_trigger() {
    for extra in [
        json!({ "qty": 10.0, "trigger_price": 1.0 }),
        json!({ "qty": 10.0, "trigger_price": 1.0, "order_type": "market" }),
        json!({ "qty": 10.0, "trigger_price": 1.0, "order_type": "MARKET" }),
    ] {
        let err = verb_from_tool_args("submit_order", &submit_args(extra.clone())).unwrap_err();
        assert_eq!(
            err,
            "submit_order: a market order takes no trigger price, it fills at the market: drop \
             the trigger price, or use a stop or take-profit order",
            "{extra}"
        );
    }
}

/// ⚠ **A trigger kind with no trigger is refused AT PARSE, before a preview token is minted** — by
/// `vike_model::order_terms_refusal`, the rule the node's door applies again. Only this grammar can
/// build one (the command lines take `--type` only with `--trigger`), and hyperliquid signed a
/// trigger-less `stop_limit` as a plain resting limit. A trigger kind's stated limit at zero or
/// below is refused the same way; a plain limit at zero is not (see the sizeable shapes above).
#[test]
fn the_mcp_submit_tool_refuses_a_trigger_kind_with_no_trigger_or_a_non_positive_limit() {
    for (extra, refusal) in [
        (json!({ "order_type": "stop" }), "a stop order needs a trigger price above zero"),
        (
            json!({ "order_type": "stop_limit", "price": 99.0 }),
            "a stop_limit order needs a trigger price above zero",
        ),
        (
            json!({ "order_type": "take_profit", "trigger_price": 0.0 }),
            "a take_profit order needs a trigger price above zero",
        ),
        (
            json!({ "order_type": "stop_limit", "trigger_price": 100.0 }),
            "a stop_limit order needs a limit price above zero",
        ),
        (
            json!({ "order_type": "take_profit", "trigger_price": 110.0, "price": 0.0 }),
            "the limit price 0 of a take_profit order is not above zero",
        ),
    ] {
        let mut server = crate::test_server();
        let err =
            server.call_tool("submit_order", &submit_args(extra.clone())).expect_err("refused");
        assert!(err.message.contains(&format!("submit_order: {refusal}")), "{extra}: {err:?}");
        assert!(server.pending.by_token.is_empty(), "{extra}: a token was minted");
    }
    for extra in [
        json!({ "order_type": "take_profit", "trigger_price": 110.0, "price": 111.0 }),
        json!({ "order_type": "stop_limit", "trigger_price": 90.0, "price": 89.0 }),
    ] {
        let verb = verb_from_tool_args("submit_order", &submit_args(extra.clone()));
        assert_matches!(verb, Ok(Verb::Submit(_)), "{extra}");
    }
}

/// An `order_type` that is not one of the platform's words is refused AT PARSE, naming the
/// accepted words — it used to be sent verbatim, and alpaca, oanda, ibkr, ig and the binance perp
/// lane turned an unknown kind into a MARKET order. No synonyms: a venue's own word is not ours.
#[test]
fn the_mcp_submit_tool_refuses_an_unknown_order_type_naming_the_accepted_words() {
    for word in ["", "garbage", "stop_market", "mkt"] {
        let args = submit_args(json!({ "order_type": word, "price": 100.0 }));
        let err = verb_from_tool_args("submit_order", &args).unwrap_err();
        assert_eq!(
            err,
            format!(
                "submit_order: `order_type` — unknown order type {word:?}: expected one of market, \
                 limit, stop, stop_limit, take_profit, market_to_limit, trailing, trailing_limit, pegged, \
                 trailing_take_profit"
            ),
        );
        assert!(wire_command_for("submit_order", &args).is_err(), "{word:?}");
    }
}

/// A known word in any casing (or padded) is accepted, and travels on the wire as its CANONICAL
/// word — the node lowers it through the same door, so the two never disagree about the kind.
#[test]
fn the_mcp_submit_tool_sends_the_canonical_order_type_word() {
    for (word, canonical) in
        [("LIMIT", "limit"), (" Limit ", "limit"), ("Stop_Limit", "stop_limit")]
    {
        let args =
            submit_args(json!({ "order_type": word, "price": 100.0, "trigger_price": 90.0 }));
        let Verb::Submit(req) = verb_from_tool_args("submit_order", &args).unwrap() else {
            panic!("{word:?} must build a submit");
        };
        assert_eq!(req.order_type, canonical, "{word:?}");
    }
    let args = submit_args(json!({ "order_type": " Market " }));
    let Verb::Submit(req) = verb_from_tool_args("submit_order", &args).unwrap() else {
        panic!("a market order must build a submit");
    };
    assert_eq!(req.order_type, "market");
    let Verb::Submit(req) = verb_from_tool_args("submit_order", &submit_args(json!({}))).unwrap()
    else {
        panic!("the default must build a submit");
    };
    assert_eq!(req.order_type, "market", "an absent order_type is still a market order");
}

/// The MCP tools that name a venue take an optional `account`, and it rides the wire command
/// exactly as the one-shot verb's labelled book does. An absent one stays ABSENT (the default is
/// unchanged), `DEFAULT` is carried as the deliberate name of the unlabelled account, and a label
/// the wire grammar refuses is refused HERE in the tool's own words.
#[test]
fn the_mcp_account_argument_reaches_the_wire_and_an_absent_one_stays_absent() {
    let tools = [
        ("submit_order", submit_args(json!({}))),
        ("flatten", json!({ "venue": "hyperliquid", "symbol": "ETH" })),
        ("market_exit", json!({ "venue": "hyperliquid" })),
        ("mass_cancel", json!({ "venue": "hyperliquid" })),
        (
            "mount_strategy",
            json!({ "venue": "hyperliquid", "symbol": "ETH", "interval": "1m", "name": "buy_hold" }),
        ),
    ];
    let account_of = |tool: &str, args: &Value| match wire_command_for(tool, args) {
        Ok(
            CommandWire::Submit(OrderRequestWire { account, .. })
            | CommandWire::Flatten { account, .. }
            | CommandWire::MarketExit { account, .. }
            | CommandWire::MassCancel { account, .. }
            | CommandWire::MountStrategy { account, .. },
        ) => Ok(account),
        Ok(other) => panic!("{tool} built {other:?}"),
        Err(e) => Err(e),
    };
    for (tool, args) in tools {
        assert_eq!(account_of(tool, &args), Ok(None), "{tool}: absent stays absent");
        for named in ["LIVE", "DEFAULT"] {
            let mut with = args.clone();
            with["account"] = json!(named);
            assert_eq!(account_of(tool, &with), Ok(Some(named.to_string())), "{tool}: {named}");
        }
        for bad in ["live", "", "A B"] {
            let mut with = args.clone();
            with["account"] = json!(bad);
            let err = account_of(tool, &with).expect_err(bad);
            assert!(err.starts_with(&format!("{tool}: `account` — ")), "{tool}: {bad:?}: {err}");
        }
    }
}

/// **A non-text `account` is REFUSED, never read as absent.** `market_exit` and `mass_cancel` with
/// no venue mean EVERY engine, so `{"account": ["ALT"]}` quietly read as "no account" would turn a
/// request for one book into the widest action the node can take. An explicit `null` is the one
/// non-text value that still means "none named" (an agent filling an optional field).
#[test]
fn a_non_text_account_is_refused_on_every_tool_and_null_is_absent() {
    for (tool, args) in [
        ("submit_order", submit_args(json!({}))),
        ("flatten", json!({ "venue": "hyperliquid", "symbol": "ETH" })),
        ("market_exit", json!({})),
        ("mass_cancel", json!({})),
        (
            "mount_strategy",
            json!({ "venue": "hyperliquid", "symbol": "ETH", "interval": "1m", "name": "buy_hold" }),
        ),
    ] {
        for bad in [json!(["ALT"]), json!(5), json!(true), json!({ "label": "ALT" })] {
            let mut with = args.clone();
            with["account"] = bad.clone();
            let err = wire_command_for(tool, &with).expect_err(&format!("{tool}: {bad}"));
            assert!(err.starts_with(&format!("{tool}: `account` must be text")), "{tool}: {err}");
            assert!(err.contains(&bad.to_string()), "the refusal echoes what it got: {err}");
        }
        let mut with = args.clone();
        with["account"] = Value::Null;
        assert!(wire_command_for(tool, &with).is_ok(), "{tool}: null is absent");
    }
}

/// The values of one JSON type's argument that are NOT of that type. `set_setting`'s `value` is
/// declared text and renders a number or a boolean ([`setting_value`]), so only a structure is wrong.
fn wrongly_typed(tool: &str, arg: &str, kind: &str) -> Vec<Value> {
    let structures = [json!(["x"]), json!({ "a": 1 })];
    let scalars = match (tool, arg, kind) {
        ("set_setting", "value", _) => vec![],
        (_, _, "string") => vec![json!(5), json!(true)],
        (_, _, "number") => vec![json!("5"), json!(true)],
        (_, _, "integer") => vec![json!("1"), json!(true), json!(1.5), json!(4_294_967_297_i64)],
        (_, _, "boolean") => vec![json!("true"), json!(1)],
        (_, _, "object") => return vec![json!(5), json!("x"), json!(true), json!([1])],
        // An array of objects (`submit_combo`'s legs): a scalar, an object, and an array of the wrong items.
        (_, _, "array") => {
            return vec![json!(5), json!("x"), json!(true), json!({ "a": 1 }), json!(["x"])];
        }
        _ => panic!("{tool}.{arg}: no wrong values for a {kind} argument"),
    };
    scalars.into_iter().chain(structures).collect()
}

/// **A present value of the wrong type is REFUSED, on every argument of every write tool**, naming the
/// tool and the argument, and an explicit `null` reads as the key being absent. The table is
/// [`write_tool_arguments`] crossed with each tool's own `inputSchema`, so a new argument is a new row:
/// one read with a bare `args.get` fails here. `set_setting`'s `value` keeps its own `null` refusal.
#[test]
fn a_wrongly_typed_argument_of_any_write_tool_is_refused_naming_both() {
    let spec = crate::tool_schemas::tools_spec();
    let mut rows = 0;
    for (tool, base) in write_tool_arguments() {
        let schema = spec.as_array().unwrap().iter().find(|t| t["name"] == tool).expect(tool);
        let properties = schema["inputSchema"]["properties"].as_object().expect("an object");
        for (arg, property) in properties {
            // The gate's own three, read beside the command and never in the function under test.
            if ["reason", "confirm", "preview_token"].contains(&arg.as_str()) {
                continue;
            }
            rows += 1;
            let kind = property["type"].as_str().unwrap_or_else(|| panic!("{tool}.{arg}: a type"));
            for bad in wrongly_typed(tool, arg, kind) {
                let mut args = base.clone();
                args[arg.as_str()] = bad.clone();
                let err = wire_command_for(tool, &args)
                    .expect_err(&format!("{tool}: `{arg}` = {bad} must be refused"));
                assert!(err.contains(tool) && err.contains(&format!("`{arg}`")), "{err}");
            }
            if (tool, arg.as_str()) == ("set_setting", "value") {
                continue;
            }
            let mut null = base.clone();
            null[arg.as_str()] = Value::Null;
            let mut absent = base.clone();
            absent.as_object_mut().expect("an object").remove(arg);
            assert_eq!(
                wire_command_for(tool, &null),
                wire_command_for(tool, &absent),
                "{tool}: `{arg}` = null reads as absent"
            );
        }
    }
    assert!(rows >= 34, "the table went empty: {rows} arguments");
}

/// What each wrong read used to do, end to end through the gate: a wider command, or a different one,
/// and a preview token to confirm it with. No token may be minted, and the blanks are refused too.
#[test]
fn a_wrong_type_or_a_blank_scope_mints_no_preview_token() {
    for (tool, args, refusal) in [
        ("market_exit", json!({ "venue": ["binance"] }), "market_exit: `venue` must be text"),
        ("market_exit", json!({ "venue": 5 }), "market_exit: `venue` must be text"),
        ("market_exit", json!({ "venue": "" }), "market_exit: `venue` is blank"),
        ("market_exit", json!({ "venue": "  " }), "market_exit: `venue` is blank"),
        ("mass_cancel", json!({ "venue": "" }), "mass_cancel: `venue` is blank"),
        (
            "mass_cancel",
            json!({ "venue": "binance", "symbol": ["BTC"] }),
            "mass_cancel: `symbol` must be text",
        ),
        ("mass_cancel", json!({ "venue": "binance", "symbol": "" }), "`symbol` is blank"),
        ("mass_cancel", json!({ "venue": "binance", "symbol": "\t" }), "`symbol` is blank"),
        ("submit_order", submit_args(json!({ "symbol": "" })), "submit_order: `symbol` is blank"),
        ("submit_order", submit_args(json!({ "venue": " " })), "submit_order: `venue` is blank"),
        ("flatten", json!({ "venue": "binance", "symbol": "" }), "flatten: `symbol` is blank"),
        ("flatten", json!({ "venue": "", "symbol": "BTCUSDT" }), "flatten: `venue` is blank"),
        (
            "submit_order",
            submit_args(json!({ "reduce_only": "true" })),
            "submit_order: `reduce_only` must be true or false",
        ),
        (
            "submit_order",
            submit_args(json!({ "order_type": 5 })),
            "submit_order: `order_type` must be text",
        ),
        (
            "submit_order",
            submit_args(json!({ "order_type": "stop", "trigger_price": "95" })),
            "submit_order: `trigger_price` must be a number",
        ),
        (
            "modify",
            json!({ "order_client_id": "c-1", "new_qty": "5", "new_price": 9 }),
            "modify: `new_qty` must be a number",
        ),
    ] {
        let mut server = crate::test_server();
        let err = server.call_tool(tool, &args).expect_err(&format!("{tool} {args}"));
        assert!(err.message.contains(refusal), "{tool} {args}: {}", err.message);
        assert!(server.pending.by_token.is_empty(), "{tool} {args}: a token was minted");
    }
}

/// The echo of what was sent is the transcript's `detail`, which `crate::trace` does not redact, so
/// the refusal must not carry a credential the transcript's `args` would have hidden.
#[test]
fn a_refusal_does_not_echo_a_credential_shaped_value() {
    let args = json!({ "venue": ["BINANCE_LIVE_API_SECRET=PLANTED"] });
    let err = wire_command_for("market_exit", &args).unwrap_err();
    assert!(!err.contains("PLANTED"), "{err}");
}

/// A `side` is 1 or -1. Measured on the running node: `side: 0` came back with a token and the node's
/// dry-run saying `accepted`, so nothing behind this reader refuses it before a confirm.
#[test]
fn a_side_that_is_neither_buy_nor_sell_is_refused() {
    for side in [0, 2, -2] {
        let err = verb_from_tool_args("submit_order", &submit_args(json!({ "side": side })))
            .expect_err(&format!("side {side} must be refused"));
        assert!(err.contains("`side`") && err.contains(&side.to_string()), "{err}");
    }
    for side in [1, -1] {
        let args = submit_args(json!({ "side": side }));
        assert!(verb_from_tool_args("submit_order", &args).is_ok(), "side {side}");
    }
}

/// A cancel or a modify must name its order, so a blank id is refused before a token exists. A
/// submit's id is optional, so a blank one reads as omitted and the minter names the order.
#[test]
fn a_blank_order_id_names_no_order() {
    for tool in ["cancel_order", "modify"] {
        for blank in ["", "   "] {
            let args = json!({ "order_client_id": blank, "new_price": 9.0 });
            let err = verb_from_tool_args(tool, &args)
                .expect_err(&format!("{tool}: {blank:?} must be refused"));
            assert!(err.contains(tool) && err.contains("blank"), "{err}");
        }
    }
    let args = submit_args(json!({ "order_client_id": "   " }));
    let Verb::Submit(req) = verb_from_tool_args("submit_order", &args).unwrap() else {
        panic!("a submit")
    };
    assert_eq!(req.order_client_id, "", "the minter fills an EMPTY id, and only an empty one");
}

/// `submit_order_list` builds, per `shape`, the SAME list the CLI's `trade order oco|oto|ladder` builds (the one
/// `vike_client::verbs` builder each, a ladder in the bracket's `stop_loss` / `take_profit` words), takes the
/// caller's leg ids one per leg, and refuses an argument its shape does not take rather than ignore it.
#[test]
fn submit_order_list_builds_the_cli_verbs_lists_and_refuses_a_stray_argument() {
    use vike_client::verbs::{
        OrderListHead, ladder_list, oco_list, oto_list, with_leg_order_client_ids,
    };
    let head = |side| OrderListHead {
        venue: "sim".into(),
        account: Some("ALT".into()),
        symbol: "B".into(),
        side,
        qty: 3.0,
    };
    let base = |extra: Value| {
        let mut args = json!({ "venue": "sim", "account": "ALT", "symbol": "B", "qty": 3.0 });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };
    let built = |args: Value| match verb_from_tool_args("submit_order_list", &args) {
        Ok(Verb::OrderList(l)) => Ok(l),
        Ok(other) => panic!("not an order list: {other:?}"),
        Err(e) => Err(e),
    };
    assert_eq!(
        built(base(json!({ "side": -1, "shape": "oco", "limit_price": 110.0, "stop_price": 90.0,
                           "stop_limit_price": 89.0, "reduce_only": true }))),
        oco_list(head(-1), 110.0, 90.0, Some(89.0), true)
    );
    assert_eq!(
        built(base(json!({ "side": 1, "shape": "oto", "entry_price": 100.0, "stop_price": 90.0 }))),
        oto_list(head(1), Some(100.0), Some(90.0), None)
    );
    assert_eq!(
        built(base(json!({ "side": 1, "shape": "ladder", "stop_loss": 90.0,
                           "take_profit": "110:1, 120:2" }))),
        ladder_list(head(1), None, 90.0, &[(110.0, 1.0), (120.0, 2.0)])
    );
    let ids = ["e1", "s1"].map(String::from);
    assert_eq!(
        built(base(json!({ "side": 1, "shape": "oto", "stop_price": 90.0,
                           "order_client_ids": "e1, s1" }))),
        with_leg_order_client_ids(oto_list(head(1), None, Some(90.0), None).unwrap(), &ids),
        "the caller's ids, one per leg in leg order"
    );
    for (args, said) in [
        (base(json!({ "side": 1, "shape": "oco", "stop_price": 90.0 })), "requires `limit_price`"),
        (
            base(json!({ "side": 1, "shape": "oco", "limit_price": 110.0, "stop_price": 90.0,
                         "take_profit": "1:1" })),
            "`take_profit` is not an argument of shape oco",
        ),
        (
            base(json!({ "side": 1, "shape": "ladder", "stop_price": 90.0,
                         "take_profit": "110:1" })),
            "`stop_price` is not an argument of shape ladder",
        ),
        (base(json!({ "side": 1, "shape": "oto" })), "exactly one exit"),
        (
            base(json!({ "side": 1, "shape": "ladder", "stop_loss": 90.0 })),
            "requires `take_profit`",
        ),
        (
            base(json!({ "side": 1, "shape": "ladder", "take_profit": "110:1" })),
            "requires `stop_loss`",
        ),
        (
            base(json!({ "side": 1, "shape": "ladder", "stop_loss": 90.0, "take_profit": "110" })),
            "price:qty",
        ),
        (
            base(json!({ "side": 1, "shape": "ladder", "stop_loss": 90.0,
                         "take_profit": "110:9" })),
            "past the entry's size",
        ),
        (
            base(
                json!({ "side": 1, "shape": "oto", "stop_price": 90.0, "order_client_ids": "e1" }),
            ),
            "1 order client ids for 2 legs",
        ),
        (base(json!({ "side": 1, "shape": "bracket" })), "oco | oto | ladder"),
    ] {
        let err = built(args).expect_err(said);
        assert!(err.starts_with("submit_order_list: ") && err.contains(said), "{said}: {err}");
    }
}

/// `submit_combo` builds the SAME combo `vike_client::verbs::combo_of` builds for `trade order combo` (its account,
/// signed net, legs and leg limits), reads the agent's id or leaves one to mint, and refuses in words what the door refuses, a leg that is no
/// object or names a key it does not take, and a price under another name (a combo naming none is a market combo).
#[test]
fn submit_combo_builds_the_combo_builders_spec_and_refuses_a_misnamed_price() {
    use vike_client::verbs::{ComboLegArg, combo_of};
    let leg = |symbol: &str, ratio, limit| ComboLegArg { symbol: symbol.into(), ratio, limit };
    let base = |extra: Value| {
        let mut args = json!({ "venue": "deribit", "account": "ALT", "side": 1, "qty": 2.0 });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };
    let built = |args: Value| match verb_from_tool_args("submit_combo", &args) {
        Ok(Verb::Combo(c)) => Ok(c),
        Ok(other) => panic!("not a combo: {other:?}"),
        Err(e) => Err(e),
    };
    let net = base(json!({ "net_limit": -0.03,
                           "legs": [{ "symbol": "C1", "ratio": 1 }, { "symbol": "C2", "ratio": -1 }] }));
    let want = combo_of(
        "deribit".into(),
        Some("ALT".into()),
        1,
        2.0,
        vec![leg("C1", 1, None), leg("C2", -1, None)],
        Some(-0.03),
        vike_model::TimeInForce::Gtc,
    );
    assert_eq!(built(net.clone()), want, "a credit net, signed");
    let by_legs = base(json!({ "legs": [{ "symbol": "C1", "ratio": 1, "limit": 0.05 },
                                        { "symbol": "C2", "ratio": -1, "limit": 0.02 }] }));
    let want = combo_of(
        "deribit".into(),
        Some("ALT".into()),
        1,
        2.0,
        vec![leg("C1", 1, Some(0.05)), leg("C2", -1, Some(0.02))],
        None,
        vike_model::TimeInForce::Gtc,
    );
    assert_eq!(built(by_legs), want, "priced by its legs");
    let mut named = net.clone();
    named["order_client_id"] = json!("myCombo1");
    let Ok(Verb::Combo(pinned)) = verb_from_tool_args("submit_combo", &named) else {
        panic!("a named id builds")
    };
    assert_eq!(pinned.order_client_id.as_deref(), Some("myCombo1"), "the agent's id is read");
    let cmd = wire_command_for("submit_combo", &net).unwrap();
    let CommandWire::Combo(minted) = fill_order_client_id(
        cmd,
        &mut order_client_id_minter(OrderClientIdSource::Mcp),
        None,
        |_| false,
    ) else {
        panic!("a combo stays one")
    };
    assert!(minted.order_client_id.is_some(), "with none named, one id is minted");
    let with = |k: &str, v: Value| {
        let mut args = net.clone();
        args[k] = v;
        built(args)
    };
    // The lifetime in `submit_order`'s words; `gtc` is the absent wire field.
    let ioc = with("time_in_force", json!("IOC")).expect("a priced ioc combo");
    assert_eq!(ioc.time_in_force.as_deref(), Some("ioc"));
    assert_eq!(with("time_in_force", json!("gtc")).unwrap().time_in_force, None);
    let mut market = net.clone();
    market.as_object_mut().unwrap().remove("net_limit");
    market["time_in_force"] = json!("fok");
    for (err, said) in [
        (with("time_in_force", json!("forever")), "`time_in_force` — "),
        (with("time_in_force", json!("gtd")), "`gtd` needs an expiry, and a combo carries none"),
        (
            with("time_in_force", json!("post_only")),
            "submit_combo: a combo cannot be post-only: no book models a maker-only combo",
        ),
        (built(market), "belongs to a limit order"),
        (with("price", json!(0.03)), "`price` is not an argument"),
        (with("limit", json!(0.03)), "`limit` is not an argument"),
        (
            with(
                "legs",
                json!([{ "symbol": "C1", "ratio": 1, "price": 0.05 }, { "symbol": "C2", "ratio": -1 }]),
            ),
            "`legs` item 0: `price` is not a field of a leg",
        ),
        (
            with("legs", json!([{ "symbol": "C1", "ratio": 1 }, "C2"])),
            "`legs` item 1: a leg must be an object",
        ),
        (
            with("legs", json!([{ "symbol": "C1", "ratio": 1 }, { "ratio": -1 }])),
            "`legs` item 1 requires `symbol`",
        ),
        (
            with(
                "legs",
                json!([{ "symbol": "C1", "ratio": 1.5 }, { "symbol": "C2", "ratio": -1 }]),
            ),
            "`legs` item 0: `ratio` must be an integer",
        ),
        (
            with(
                "legs",
                json!([{ "symbol": "C1", "ratio": 1, "limit": 0.05 }, { "symbol": "C2", "ratio": -1 }]),
            ),
            "never both",
        ),
        (with("legs", json!([{ "symbol": "C1", "ratio": 1 }])), "needs 2..=N legs"),
        (
            with("legs", json!([{ "symbol": "C1", "ratio": 1 }, { "symbol": "C2", "ratio": 0 }])),
            "ratio 0",
        ),
    ] {
        let err = err.expect_err(said);
        assert!(err.starts_with("submit_combo: ") && err.contains(said), "{said}: {err}");
    }
}
