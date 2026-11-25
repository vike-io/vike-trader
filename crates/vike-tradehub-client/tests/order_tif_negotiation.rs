//! Feature negotiation for the order's LIFETIME: a `Submit` that names `time_in_force` (a `post-only` or
//! `ioc` order) or a `gtd_expiry` owes `order-tif`, because the node that lacks it does not fail to decode
//! the frame: it decodes it, DROPS the two keys and runs the order as a plain GTC limit behind a normal
//! acknowledgement (a post-only order that takes liquidity). The refusal is CLIENT-SIDE and the assertion
//! that carries the weight is that the scripted node recorded NOTHING after the handshake; it has a
//! complement (a submit that names no lifetime still flies) and a release (the advertised word lets the
//! lifetime fly), without which a client refusing every submit would pass.

use vike_tradehub_client::proto::FEATURE_ORDER_TIME_IN_FORCE;
use vike_tradehub_client::test_support::strategy_node::{KEY, scripted_node, todays_node_features};
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};
use vike_tradehub_client::{CommandOutcome, ControlRejected, TradehubControlHandle};

/// A limit `Submit` with the lifetime under test (`account` named or not, the axis the account word guards).
fn submit(
    time_in_force: Option<&str>,
    gtd_expiry: Option<i64>,
    account: Option<&str>,
) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-tif".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
        time_in_force: time_in_force.map(str::to_string),
        gtd_expiry,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// A node that advertises everything this tree made true EXCEPT `order-tif` (the node of the release before).
fn node_without_the_word() -> Vec<String> {
    let mut f = todays_node_features();
    f.retain(|w| w != FEATURE_ORDER_TIME_IN_FORCE);
    f
}

/// ⚠ **A lifetime is refused client-side against a node without `order-tif`**, each shape that carries one:
/// a post-only order, a plain `ioc`, a `gtd` with its expiry, and (the arm order) the same naming an account.
/// Nothing is sent.
#[test]
fn a_lifetime_is_refused_client_side_against_a_node_without_the_word() {
    for (what, cmd) in [
        ("post-only", submit(Some("post-only"), None, None)),
        ("ioc", submit(Some("ioc"), None, None)),
        ("gtd", submit(Some("gtd"), Some(4_102_444_800_000), None)),
        ("post-only on a named account", submit(Some("post-only"), None, Some("ALT"))),
    ] {
        let (addr, report) = scripted_node(node_without_the_word());
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert_eq!(
            handle.try_command(cmd),
            Err(ControlRejected::UnsupportedByHub),
            "{what} must not fly at a node that would drop it"
        );
        drop(handle);
        let seen = report.recv().expect("scripted node reports");
        assert!(seen.is_empty(), "{what}: NOTHING may be sent after auth, saw: {seen:?}");
    }
}

/// The complement: on that SAME node a submit that names no lifetime still flies, byte for byte the frame
/// it has always been (it owes no capability).
#[test]
fn a_submit_naming_no_lifetime_still_flows_to_a_node_without_the_word() {
    let (addr, report) = scripted_node(node_without_the_word());
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(submit(None, None, None)).expect("no lifetime ⇒ owes nothing");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The release: with `order-tif` advertised the lifetime flies.
#[test]
fn a_lifetime_flows_to_a_node_advertising_the_word() {
    let (addr, report) = scripted_node(todays_node_features());
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(submit(Some("post-only"), None, Some("ALT"))).expect("advertised");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The frame itself: an order that names no lifetime serialises WITHOUT the two keys (the byte-identical
/// frame an older node decodes), and one that names a lifetime carries them verbatim.
#[test]
fn the_lifetime_keys_are_on_the_wire_only_when_named() {
    let bare = serde_json::to_string(&submit(None, None, None)).expect("serialises");
    assert!(!bare.contains("time_in_force") && !bare.contains("gtd_expiry"), "{bare}");
    let named = serde_json::to_string(&submit(Some("gtd"), Some(4_102_444_800_000), None))
        .expect("serialises");
    assert!(
        named.contains("\"time_in_force\":\"gtd\"")
            && named.contains("\"gtd_expiry\":4102444800000"),
        "{named}"
    );
    let old: CommandWire = serde_json::from_str(&bare).expect("a pre-field frame still decodes");
    assert_eq!(old, submit(None, None, None));
}

/// **`takes` answers the gate without sending**: false for exactly the submits `try_command` refuses
/// against a node without the word (a post-only order, on any account), true for them against a node
/// that advertises it, and true for a submit naming no lifetime on both (it owes nothing). This is what
/// the desktop ticket asks before it enables Post only, so a control and a send cannot disagree.
#[test]
fn takes_says_what_the_gate_would_say() {
    let post_only = submit(Some("post-only"), None, None);
    let named = submit(Some("post-only"), None, Some("ALT"));
    let plain = submit(None, None, None);
    for (features, lifetime_ok) in
        [(node_without_the_word(), false), (todays_node_features(), true)]
    {
        let (addr, _report) = scripted_node(features);
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
        assert_eq!(
            handle.takes(&post_only),
            lifetime_ok,
            "post-only, lifetime word: {lifetime_ok}"
        );
        assert_eq!(handle.takes(&named), lifetime_ok, "post-only on a named account");
        assert!(handle.takes(&plain), "a submit with no lifetime owes nothing");
    }
}

// ---- the three wave-2 order FIELDS: `order-display`, `order-release`, `order-modify-trigger` ----------------------

/// A submit showing a slice, a submit held until a moment (with and without a price condition), and a modify moving a
/// trigger, each with the word it owes. A node without the word DECODES each frame and drops the key: an iceberg rests
/// wholly in view, a held order is sent AT ONCE, a trigger modify changes the rest or nothing.
fn wave_two_fields() -> [(&'static str, CommandWire, &'static str); 4] {
    use vike_tradehub_client::proto::{
        FEATURE_ORDER_DISPLAY, FEATURE_ORDER_MODIFY_TRIGGER, FEATURE_ORDER_RELEASE,
    };
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire};
    let with = |f: fn(&mut OrderRequestWire)| {
        let CommandWire::Submit(mut req) = submit(None, None, None) else { unreachable!() };
        f(&mut req);
        CommandWire::Submit(req)
    };
    [
        ("display", with(|r| r.display_quantity = Some(0.1)), FEATURE_ORDER_DISPLAY),
        (
            "release",
            with(|r| {
                r.release = Some(OrderReleaseWire {
                    at: 4_102_444_800_000,
                    price: None,
                    symbol: None,
                    venue: None,
                });
            }),
            FEATURE_ORDER_RELEASE,
        ),
        (
            "release with a price condition",
            with(|r| {
                r.release = Some(OrderReleaseWire {
                    at: 4_102_444_800_000,
                    price: Some(OrderReleasePriceWire::Above(2.0)),
                    symbol: None,
                    venue: None,
                });
            }),
            FEATURE_ORDER_RELEASE,
        ),
        (
            "modify trigger",
            CommandWire::Modify {
                order_client_id: "c-stop".into(),
                new_qty: None,
                new_price: None,
                new_trigger_price: Some(0.9),
            },
            FEATURE_ORDER_MODIFY_TRIGGER,
        ),
    ]
}

/// ⚠ **Each wave-2 field is refused client-side against a node without its word**, and nothing is sent after the
/// handshake; the same command flies to a node advertising it, and `takes` says the same before the send.
#[test]
fn each_wave_two_field_is_refused_client_side_without_its_word_and_flies_with_it() {
    for (what, cmd, word) in wave_two_fields() {
        let mut without = todays_node_features();
        without.retain(|w| w != word);
        let (addr, report) = scripted_node(without);
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert!(!handle.takes(&cmd), "{what}: takes agrees with the send");
        assert_eq!(
            handle.try_command(cmd.clone()),
            Err(ControlRejected::UnsupportedByHub),
            "{what} must not fly at a node that would drop it"
        );
        drop(handle);
        let seen = report.recv().expect("scripted node reports");
        assert!(seen.is_empty(), "{what}: NOTHING may be sent after auth, saw: {seen:?}");

        let (addr, report) = scripted_node(todays_node_features());
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
        assert!(handle.takes(&cmd), "{what}: advertised");
        let t = handle.try_command(cmd).expect("advertised");
        let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
        assert_eq!(
            outcome,
            Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() }),
            "{what}"
        );
        drop(handle);
        assert_eq!(report.recv().expect("report"), vec!["Command".to_string()], "{what}");
    }
}

/// The frames: a submit or a modify that names none of the three fields serialises WITHOUT their keys (the
/// byte-identical frame an older node decodes), and one that names them carries them in the wire's spelling; a
/// release key the node does not know is REFUSED on decode, never dropped.
#[test]
fn the_wave_two_keys_are_on_the_wire_only_when_named() {
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire};
    let bare = serde_json::to_string(&submit(None, None, None)).expect("serialises");
    assert!(!bare.contains("display_quantity") && !bare.contains("release"), "{bare}");
    let plain_modify = CommandWire::Modify {
        order_client_id: "c".into(),
        new_qty: Some(1.0),
        new_price: None,
        new_trigger_price: None,
    };
    let modify = serde_json::to_string(&plain_modify).expect("serialises");
    assert_eq!(modify, r#"{"Modify":{"order_client_id":"c","new_qty":1.0,"new_price":null}}"#);
    let [_, _, (_, conditional, _), (_, trigger, _)] = wave_two_fields();
    let conditional = serde_json::to_string(&conditional).expect("serialises");
    assert!(
        conditional.contains(r#""release":{"at":4102444800000,"price":{"above":2.0}}"#),
        "{conditional}"
    );
    // A condition on another symbol names it beside the price, and decodes back to the same release.
    let other = OrderReleaseWire {
        at: 1,
        price: Some(OrderReleasePriceWire::Below(9.0)),
        symbol: Some("ETHUSDT".into()),
        venue: None,
    };
    let bytes = serde_json::to_string(&other).expect("serialises");
    assert_eq!(bytes, r#"{"at":1,"price":{"below":9.0},"symbol":"ETHUSDT"}"#);
    assert_eq!(serde_json::from_str::<OrderReleaseWire>(&bytes).expect("decodes"), other);
    let trigger = serde_json::to_string(&trigger).expect("serialises");
    assert!(trigger.contains(r#""new_trigger_price":0.9"#), "{trigger}");
    // The bare order's first `}` closes it (every field before it is a scalar).
    let unknown = bare.replacen('}', r#","release":{"at":1,"volume":5}}"#, 1);
    assert!(
        serde_json::from_str::<CommandWire>(&unknown).is_err(),
        "a condition this build does not know is refused, not dropped: {unknown}"
    );
}

// ---- the wave-3 order FIELDS: `order-trailing` -------------------------------------------------------------------

/// A trailing stop (a trail) and a trailing limit (a trail and an offset), the second ALSO held until a moment: one
/// word covers both fields and the release beside them (`order-trailing` ships in the build after `order-release`).
fn trailing(kind: &str, limit_offset: Option<f64>, held: bool) -> CommandWire {
    let CommandWire::Submit(req) = submit(None, None, None) else { unreachable!() };
    CommandWire::Submit(OrderRequestWire {
        order_type: kind.into(),
        price: None,
        trail: Some(5.0),
        limit_offset,
        release: held.then_some(vike_tradehub_client::wire::OrderReleaseWire {
            at: 4_102_444_800_000,
            price: None,
            symbol: None,
            venue: None,
        }),
        ..req
    })
}

/// ⚠ **A trail or a limit offset is refused client-side against a node without `order-trailing`** (it would drop
/// both keys behind a decode that succeeds), nothing is sent, and the same submit flies to a node advertising it.
#[test]
fn a_trailing_submit_is_refused_client_side_without_its_word_and_flies_with_it() {
    use vike_tradehub_client::proto::FEATURE_ORDER_TRAILING;
    for (what, cmd) in [
        ("trailing", trailing("trailing", None, false)),
        ("trailing_limit, held", trailing("trailing_limit", Some(0.5), true)),
    ] {
        let mut without = todays_node_features();
        without.retain(|w| w != FEATURE_ORDER_TRAILING);
        let (addr, report) = scripted_node(without);
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert!(!handle.takes(&cmd), "{what}: takes agrees with the send");
        assert_eq!(
            handle.try_command(cmd.clone()),
            Err(ControlRejected::UnsupportedByHub),
            "{what}"
        );
        drop(handle);
        assert!(
            report.recv().expect("report").is_empty(),
            "{what}: NOTHING may be sent after auth"
        );

        let (addr, report) = scripted_node(todays_node_features());
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
        let t = handle.try_command(cmd).expect("advertised");
        let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
        assert_eq!(
            outcome,
            Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() }),
            "{what}"
        );
        drop(handle);
        assert_eq!(report.recv().expect("report"), vec!["Command".to_string()], "{what}");
    }
}

/// The frames: an order naming neither field carries neither key (the frame from before), and a trailing limit
/// carries both in the wire's spelling.
#[test]
fn the_trailing_keys_are_on_the_wire_only_when_named() {
    let bare = serde_json::to_string(&submit(None, None, None)).expect("serialises");
    assert!(!bare.contains("trail") && !bare.contains("limit_offset"), "{bare}");
    let named =
        serde_json::to_string(&trailing("trailing_limit", Some(0.5), false)).expect("serialises");
    assert!(named.contains(r#""trail":5.0,"limit_offset":0.5"#), "{named}");
}

// ---- the wave-5 order FIELD: `order-peg` -------------------------------------------------------------------------

/// ⚠ **A peg is refused client-side against a node without `order-peg`** (it would drop the key behind a decode that
/// succeeds), nothing is sent, the same submit flies to a node advertising it, and a submit naming no peg carries no
/// key (the frame from before).
#[test]
fn a_pegged_submit_is_refused_client_side_without_its_word_and_flies_with_it() {
    use vike_tradehub_client::proto::FEATURE_ORDER_PEG;
    use vike_tradehub_client::wire::OrderPegWire;
    let CommandWire::Submit(req) = submit(None, None, None) else { unreachable!() };
    let cmd = CommandWire::Submit(OrderRequestWire {
        order_type: "pegged".into(),
        price: None,
        peg: Some(OrderPegWire { anchor: "mid".into(), offset: -0.05 }),
        ..req
    });
    let named = serde_json::to_string(&cmd).expect("serialises");
    assert!(named.contains(r#""peg":{"anchor":"mid","offset":-0.05}"#), "{named}");
    let bare = serde_json::to_string(&submit(None, None, None)).expect("serialises");
    assert!(!bare.contains("peg"), "{bare}");

    let mut without = todays_node_features();
    without.retain(|w| w != FEATURE_ORDER_PEG);
    let (addr, report) = scripted_node(without);
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert!(!handle.takes(&cmd), "takes agrees with the send");
    assert_eq!(handle.try_command(cmd.clone()), Err(ControlRejected::UnsupportedByHub));
    drop(handle);
    assert!(report.recv().expect("report").is_empty(), "NOTHING may be sent after auth");

    let (addr, report) = scripted_node(todays_node_features());
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(cmd).expect("advertised");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

// ---- the attributes wave's order FIELDS: `order-attributes` --------------------------------------------------------

/// ⚠ **Each attributes-wave term is refused client-side against a node without `order-attributes`** (an older node
/// drops the three keys behind a decode that succeeds and sends a plain order), nothing is sent, the same submit flies
/// to a node advertising it, and a submit naming none carries no key (the frame from before). A release's price VENUE
/// owes the word too; a release reading the order's own venue owes only `order-release`.
#[test]
fn each_attributes_term_is_refused_client_side_without_its_word_and_flies_with_it() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    use vike_tradehub_client::proto::{FEATURE_ORDER_ATTRIBUTES, FEATURE_ORDER_RELEASE};
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire};
    let CommandWire::Submit(req) = submit(None, None, None) else { unreachable!() };
    let bare = serde_json::to_string(&submit(None, None, None)).expect("serialises");
    for key in ["attributes", "trail_percent", "adjust", "\"release\""] {
        assert!(!bare.contains(key), "{key} absent on a plain submit: {bare}");
    }
    let release = |venue: Option<&str>| OrderReleaseWire {
        at: 4_097_217_600_000,
        price: Some(OrderReleasePriceWire::Above(100_000.0)),
        symbol: Some("BTCUSDT".into()),
        venue: venue.map(Into::into),
    };
    let cases = [
        (
            OrderRequestWire {
                attributes: Some(OrderAttributes { all_or_none: true, ..Default::default() }),
                ..req.clone()
            },
            r#""attributes":{"all_or_none":true}"#,
        ),
        (
            OrderRequestWire {
                order_type: "trailing".into(),
                price: None,
                trail_percent: Some(1.5),
                ..req.clone()
            },
            r#""trail_percent":1.5"#,
        ),
        (
            OrderRequestWire {
                order_type: "stop".into(),
                price: None,
                trigger_price: Some(90.0),
                adjust: Some(OrderStopAdjust {
                    at: 120.0,
                    to: OrderStopAdjustTo::Trailing {
                        stop: 110.0,
                        trail: None,
                        trail_percent: Some(2.0),
                    },
                }),
                ..req.clone()
            },
            r#""adjust":{"at":120.0,"to":{"trailing":{"stop":110.0,"trail_percent":2.0}}}"#,
        ),
        (
            OrderRequestWire { release: Some(release(Some("binance"))), ..req.clone() },
            r#""venue":"binance""#,
        ),
    ];
    for (req, key) in cases {
        let cmd = CommandWire::Submit(req);
        let named = serde_json::to_string(&cmd).expect("serialises");
        assert!(named.contains(key), "{key} in {named}");

        let mut without = todays_node_features();
        without.retain(|w| w != FEATURE_ORDER_ATTRIBUTES);
        let (addr, report) = scripted_node(without);
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert!(!handle.takes(&cmd), "takes agrees with the send: {key}");
        assert_eq!(
            handle.try_command(cmd.clone()),
            Err(ControlRejected::UnsupportedByHub),
            "{key}"
        );
        drop(handle);
        assert!(report.recv().expect("report").is_empty(), "NOTHING may be sent after auth: {key}");

        let (addr, report) = scripted_node(todays_node_features());
        let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
        let t = handle.try_command(cmd).expect("advertised");
        let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
        assert_eq!(
            outcome,
            Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() }),
            "{key}"
        );
        drop(handle);
        assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
    }
    // A condition on the order's own venue is the frame from before: it owes `order-release` alone.
    let own = CommandWire::Submit(OrderRequestWire { release: Some(release(None)), ..req });
    let mut without = todays_node_features();
    without.retain(|w| w != FEATURE_ORDER_ATTRIBUTES);
    let (addr, _report) = scripted_node(without);
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
    assert!(handle.takes(&own), "an own-venue condition owes {FEATURE_ORDER_RELEASE} only");
}

// ---- the wave-4 ORDER LIST: `order-list` ---------------------------------------------------------------------------

/// A two-leg OCO on a named account: a take-profit limit and a protective stop, each cancelling the other.
fn oco() -> CommandWire {
    use vike_model::ContingencyType;
    use vike_tradehub_client::wire::{OrderListLegWire, OrderListWire};
    let leg =
        |order_type: &str, price, trigger_price, linked, order_client_id: &str| OrderListLegWire {
            side: -1,
            qty: 1.0,
            order_type: order_type.into(),
            price,
            trigger_price,
            trail: None,
            reduce_only: true,
            time_in_force: None,
            parent: None,
            contingency: Some(ContingencyType::Oco),
            linked: vec![linked],
            order_client_id: Some(order_client_id.into()),
        };
    CommandWire::OrderList(OrderListWire {
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        legs: vec![
            leg("limit", Some(110.0), None, 1, "c1tp"),
            leg("stop", None, Some(90.0), 0, "c1sl"),
        ],
        account: Some("ALT".into()),
    })
}

/// ⚠ **An order list is refused client-side against a node without `order-list`** (it cannot decode the variant),
/// nothing is sent, and the same list flies to a node advertising it.
#[test]
fn an_order_list_is_refused_client_side_without_its_word_and_flies_with_it() {
    use vike_tradehub_client::proto::FEATURE_ORDER_LIST;
    let mut without = todays_node_features();
    without.retain(|w| w != FEATURE_ORDER_LIST);
    let (addr, report) = scripted_node(without);
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert!(!handle.takes(&oco()), "takes agrees with the send");
    assert_eq!(handle.try_command(oco()), Err(ControlRejected::UnsupportedByHub));
    drop(handle);
    assert!(report.recv().expect("report").is_empty(), "NOTHING may be sent after auth");

    let (addr, report) = scripted_node(todays_node_features());
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(oco()).expect("advertised");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The frame: a leg writes only what it names, a key the node does not know is refused on decode (never dropped), and
/// a fourth contingency word does not decode.
#[test]
fn the_order_list_frame_is_pinned_and_refuses_what_it_does_not_know() {
    let frame = serde_json::to_string(&oco()).expect("serialises");
    assert_eq!(
        frame,
        r#"{"OrderList":{"venue":"okx","symbol":"BTC-USDT-SWAP","legs":[{"side":-1,"qty":1.0,"order_type":"limit","price":110.0,"reduce_only":true,"contingency":"OCO","linked":[1],"order_client_id":"c1tp"},{"side":-1,"qty":1.0,"order_type":"stop","trigger_price":90.0,"reduce_only":true,"contingency":"OCO","linked":[0],"order_client_id":"c1sl"}],"account":"ALT"}}"#
    );
    assert_eq!(serde_json::from_str::<CommandWire>(&frame).expect("round trips"), oco());
    let unknown_leg_key = frame.replacen(r#""side":-1,"#, r#""side":-1,"iceberg":2.0,"#, 1);
    assert!(serde_json::from_str::<CommandWire>(&unknown_leg_key).is_err(), "a leg key is refused");
    let unknown_key = frame.replacen(r#""venue":"okx","#, r#""venue":"okx","list_id":"x","#, 1);
    assert!(serde_json::from_str::<CommandWire>(&unknown_key).is_err(), "a list key is refused");
    let fourth = frame.replacen(r#""OCO""#, r#""OTOCO""#, 1);
    assert!(serde_json::from_str::<CommandWire>(&fourth).is_err(), "a fourth word is refused");
    // A trailing leg's trail rides the leg; the pinned frame above, which names none, writes no key.
    let trailing =
        frame.replacen(r#""order_type":"stop","#, r#""order_type":"trailing","trail":5.0,"#, 1);
    let Ok(CommandWire::OrderList(list)) = serde_json::from_str::<CommandWire>(&trailing) else {
        panic!("a leg's trail decodes: {trailing}");
    };
    assert_eq!((list.legs[0].trail, list.legs[1].trail), (None, Some(5.0)));
    assert!(list.say().contains("leg 1 sell 1 trailing trigger=90 trail=5"), "{}", list.say());
}

// ---- the wave-6 COMBO: `combo` -------------------------------------------------------------------------------------

/// A two-leg call spread on a named account, priced by its legs when it names no net.
fn combo(net_limit: Option<f64>) -> CommandWire {
    use vike_model::OrderComboLeg;
    use vike_tradehub_client::wire::OrderComboWire;
    let leg = |symbol: &str, ratio, limit| OrderComboLeg { symbol: symbol.into(), ratio, limit };
    let by_legs = net_limit.is_none();
    CommandWire::Combo(OrderComboWire {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            leg("BTC-27DEC26-100000-C", 1, by_legs.then_some(0.05)),
            leg("BTC-27DEC26-110000-C", -1, by_legs.then_some(0.02)),
        ],
        net_limit,
        account: Some("ALT".into()),
        time_in_force: None,
        order_client_id: Some("c1combo".into()),
    })
}

/// ⚠ **A combo is refused client-side against a node without `combo`** (it cannot decode the variant), nothing is sent,
/// and the same combo flies to a node advertising it.
#[test]
fn a_combo_is_refused_client_side_without_its_word_and_flies_with_it() {
    use vike_tradehub_client::proto::FEATURE_COMBO;
    assert_eq!(FEATURE_COMBO, "combo");
    let mut without = todays_node_features();
    without.retain(|w| w != FEATURE_COMBO);
    let (addr, report) = scripted_node(without);
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert!(!handle.takes(&combo(None)), "takes agrees with the send");
    assert_eq!(handle.try_command(combo(None)), Err(ControlRejected::UnsupportedByHub));
    drop(handle);
    assert!(report.recv().expect("report").is_empty(), "NOTHING may be sent after auth");

    let (addr, report) = scripted_node(todays_node_features());
    let handle = TradehubControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(combo(Some(0.03))).expect("advertised");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(CommandOutcome::Accepted { order_client_id: String::new(), notes: Vec::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The frame: a leg writes its limit only when it names one, the net is ALWAYS written (`null` is the only spelling of
/// none, and an omitted net is refused rather than read as a market combo), the lifetime only when named and the id
/// last, and a key the node does not know is refused on decode, never dropped.
#[test]
fn the_combo_frame_is_pinned_and_refuses_what_it_does_not_know() {
    let frame = serde_json::to_string(&combo(None)).expect("serialises");
    assert_eq!(
        frame,
        r#"{"Combo":{"venue":"deribit","side":1,"qty":2.0,"legs":[{"symbol":"BTC-27DEC26-100000-C","ratio":1,"limit":0.05},{"symbol":"BTC-27DEC26-110000-C","ratio":-1,"limit":0.02}],"net_limit":null,"account":"ALT","order_client_id":"c1combo"}}"#
    );
    assert_eq!(serde_json::from_str::<CommandWire>(&frame).expect("round trips"), combo(None));
    let CommandWire::Combo(mut ioc) = combo(Some(0.03)) else { unreachable!() };
    ioc.time_in_force = Some("ioc".into());
    let lifetime = serde_json::to_string(&CommandWire::Combo(ioc.clone())).expect("serialises");
    assert!(
        lifetime
            .ends_with(r#""account":"ALT","time_in_force":"ioc","order_client_id":"c1combo"}}"#)
    );
    assert_eq!(
        serde_json::from_str::<CommandWire>(&lifetime).expect("round trips"),
        CommandWire::Combo(ioc)
    );
    let net = serde_json::to_string(&combo(Some(-0.03))).expect("serialises");
    assert!(net.contains(r#""net_limit":-0.03"#) && !net.contains(r#""limit":0."#), "{net}");
    let omitted = frame.replacen(r#","net_limit":null"#, "", 1);
    assert!(serde_json::from_str::<CommandWire>(&omitted).is_err(), "an omitted net is refused");
    let unknown_leg_key = frame.replacen(r#""ratio":1,"#, r#""ratio":1,"side":1,"#, 1);
    assert!(serde_json::from_str::<CommandWire>(&unknown_leg_key).is_err(), "a leg key is refused");
    let unknown_key =
        frame.replacen(r#""venue":"deribit","#, r#""venue":"deribit","tif":"ioc","#, 1);
    assert!(serde_json::from_str::<CommandWire>(&unknown_key).is_err(), "a combo key is refused");
    assert!(
        combo(None).names_no_account().is_none()
            && combo(None).addressed_venue() == Some("deribit"),
        "a named combo addresses its venue and names its account"
    );
}

/// Every order shape this suite sends, and one submit naming every optional term at once, cross every session form
/// value for value: msgpack with field names keeps what each strict decoder (an order list's, a combo's) requires and
/// refuses, as JSON does.
#[test]
fn every_order_shape_round_trips_through_every_session_form() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    use vike_tradehub_client::test_support::assert_round_trips_in_every_form;
    use vike_tradehub_client::wire::{OrderPegWire, OrderReleasePriceWire, OrderReleaseWire};
    let CommandWire::Submit(req) = submit(Some("gtd"), Some(4_102_444_800_000), Some("ALT")) else {
        unreachable!()
    };
    let every_term = CommandWire::Submit(OrderRequestWire {
        trigger_price: Some(90.0),
        display_quantity: Some(0.25),
        release: Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePriceWire::Above(100_000.0)),
            symbol: Some("BTCUSDT".into()),
            venue: Some("binance".into()),
        }),
        trail: Some(5.0),
        limit_offset: Some(0.5),
        peg: Some(OrderPegWire { anchor: "mid".into(), offset: -0.05 }),
        attributes: Some(OrderAttributes { all_or_none: true, ..Default::default() }),
        trail_percent: Some(1.5),
        adjust: Some(OrderStopAdjust {
            at: 120.0,
            to: OrderStopAdjustTo::Trailing { stop: 110.0, trail: None, trail_percent: Some(2.0) },
        }),
        ..req
    });
    for cmd in [
        every_term,
        trailing("trailing_limit", Some(0.5), true),
        oco(),
        combo(None),
        combo(Some(-0.03)),
    ] {
        assert_round_trips_in_every_form(&cmd);
    }
}
