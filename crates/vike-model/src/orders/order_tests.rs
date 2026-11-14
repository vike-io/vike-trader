use super::*;
use crate::time::MS_PER_DAY;
use std::assert_matches;

/// `OrderResting` rests in the live core's conditional book too, where fees are the venue's: what
/// only a simulator needs to know about an order lives in that simulator's own side table, keyed by
/// `qid` (`vike_simulator::OrderBrokerSimulator`'s `order_fees`), never here. Growing it is a decision, not a side
/// effect: the attributes wave (decision 0267) took it from 176 to 224 bytes (the discretion and the
/// percent trail, 16 each; the price condition's venue inside `release`, 8; the adjustment BOXED, 8,
/// where inline it would cost 48; all-or-none fits the padding).
#[test]
fn a_working_order_is_224_bytes() {
    assert_eq!(std::mem::size_of::<OrderResting>(), 224);
}

/// Only a `Limit` books as a maker: a `LimitClose` has rested too, and it books as a taker, and so
/// does a stop-limit or take-profit until it rests as the `Limit` at its limit.
#[test]
fn only_a_limit_books_as_a_maker() {
    use OrderKind::{
        AuctionClose, AuctionOpen, Limit, LimitClose, Market, MarketClose, MarketToLimit, Stop,
        StopLimit, TakeProfit, Trailing, TrailingLimit,
    };
    for (kind, maker) in [
        (Market, false),
        (MarketClose, false),
        (LimitClose, false),
        (Limit, true),
        (Stop, false),
        (Trailing, false),
        (StopLimit, false),
        (TakeProfit, false),
        (MarketToLimit, false),
        (AuctionOpen, false),
        (AuctionClose, false),
        (TrailingLimit, false),
        (OrderKind::TrailingTakeProfit, false),
        (OrderKind::AuctionOpenToLimit, false),
    ] {
        assert_eq!(OrderResting::new(kind, 1, 1.0).books_as_maker(), maker, "{kind:?}");
    }
    let mut rested = OrderResting { limit: Some(99.0), ..OrderResting::new(StopLimit, 1, 1.0) };
    rested.rest_as_limit();
    assert!(rested.books_as_maker());
    // a discretionary limit takes liquidity inside its discretion: a taker
    let discretionary =
        OrderResting { discretionary_amount: Some(0.5), ..OrderResting::new(Limit, 1, 1.0) };
    assert!(!discretionary.books_as_maker());
}

/// The attributes wave's compatibility pin: a working order and a request with none of the wave's
/// keys serialize to EXACTLY the bytes they had before the fields existed (pinned against literals,
/// a release whose condition names no venue included), and each key, once set, rides LAST.
#[test]
fn working_order_without_the_attribute_fields_is_byte_identical() {
    let limit = OrderResting { price: Some(100.0), ..OrderResting::new(OrderKind::Limit, 1, 2.0) };
    let js = serde_json::to_string(&limit).unwrap();
    assert_eq!(
        js,
        r#"{"kind":"limit","side":1,"size":2.0,"price":100.0,"trail":null,"extreme":null,"weight":0.0,"stop":null}"#
    );
    let cond = OrderReleasePrice {
        symbol: ustr::Ustr::from("BTCUSDT"),
        level: 100.0,
        side: OrderReleasePriceSide::Above,
        venue: None,
    };
    let held =
        OrderResting { release: Some(OrderRelease { at: 1, price: Some(cond) }), ..limit.clone() };
    let hjs = serde_json::to_string(&held).unwrap();
    assert!(
        hjs.ends_with(
            r#""release":{"at":1,"price":{"symbol":"BTCUSDT","level":100.0,"side":"above"}}}"#
        ),
        "{hjs}"
    );
    let set = OrderResting {
        all_or_none: true,
        discretionary_amount: Some(0.5),
        trail_percent: Some(1.5),
        adjust: Some(Box::new(OrderStopAdjust {
            at: 110.0,
            to: OrderStopAdjustTo::Stop { stop: 100.0 },
        })),
        ..limit
    };
    let sjs = serde_json::to_string(&set).unwrap();
    assert!(
        sjs.ends_with(
            r#""stop":null,"all_or_none":true,"discretionary_amount":0.5,"trail_percent":1.5,"adjust":{"at":110.0,"to":{"stop":{"stop":100.0}}}}"#
        ),
        "{sjs}"
    );
    assert_eq!(serde_json::from_str::<OrderResting>(&sjs).unwrap(), set);
    for (kind, word) in [
        (OrderKind::TrailingTakeProfit, "trailing_take_profit"),
        (OrderKind::AuctionOpenToLimit, "auction_open_to_limit"),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{word}\""), "{kind:?}");
    }
}

/// The request's twin: no new key, no new byte; the three new keys ride LAST; a target or an
/// attribute set with a term this build does not know REFUSES the record (`deny_unknown_fields`), and
/// so does a price condition naming a key it does not know.
#[test]
fn order_request_without_the_attribute_keys_is_byte_identical() {
    let plain = OrderRequest {
        order_client_id: "c1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    };
    let js = serde_json::to_string(&plain).unwrap();
    for key in ["attributes", "trail_percent", "adjust"] {
        assert!(!js.contains(key), "{key} in {js}");
    }
    let attributed = OrderRequest {
        attributes: Some(OrderAttributes { all_or_none: true, ..Default::default() }),
        ..plain.clone()
    };
    let ajs = serde_json::to_string(&attributed).unwrap();
    assert!(ajs.ends_with(r#""attributes":{"all_or_none":true}}"#), "{ajs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&ajs).unwrap(), attributed);
    let trailing =
        OrderStopAdjustTo::Trailing { stop: 95.0, trail: None, trail_percent: Some(2.0) };
    let adjusted = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(90.0),
        adjust: Some(OrderStopAdjust { at: 105.0, to: trailing }),
        side: -1,
        ..plain
    };
    let djs = serde_json::to_string(&adjusted).unwrap();
    assert!(
        djs.ends_with(
            r#""adjust":{"at":105.0,"to":{"trailing":{"stop":95.0,"trail_percent":2.0}}}}"#
        ),
        "{djs}"
    );
    assert_eq!(serde_json::from_str::<OrderRequest>(&djs).unwrap(), adjusted);
    let unknown_attribute = r#"{"all_or_none":true,"iceberg":true}"#;
    assert!(serde_json::from_str::<OrderAttributes>(unknown_attribute).is_err());
    for bad in [
        r#"{"at":105.0,"to":{"stop":{"stop":100.0,"limit":99.0}}}"#,
        r#"{"at":105.0,"to":{"bracket":{"stop":100.0}}}"#,
        r#"{"at":105.0,"to":{"stop":{"stop":100.0}},"once":true}"#,
    ] {
        assert!(serde_json::from_str::<OrderStopAdjust>(bad).is_err(), "{bad}");
    }
    let unknown = r#"{"symbol":"BTCUSDT","level":1.0,"side":"above","feed":"x"}"#;
    assert!(serde_json::from_str::<OrderReleasePrice>(unknown).is_err());
    let on = r#"{"symbol":"BTCUSDT","level":1.0,"side":"above","venue":"binance"}"#;
    let read: OrderReleasePrice = serde_json::from_str(on).unwrap();
    assert_eq!(read.venue.map(|v| v.to_string()).as_deref(), Some("binance"));
    assert_eq!(serde_json::to_string(&read).unwrap(), on, "a venue rides last");
}

/// The limit compatibility pin: a working order with no limit serializes to EXACTLY the bytes it had
/// before the field existed (pinned against a literal), an old body reads back with no limit, and a
/// stop-limit carries its limit after the fields that were there.
#[test]
fn working_order_without_limit_is_byte_identical() {
    let stop = OrderResting { price: Some(100.0), ..OrderResting::new(OrderKind::Stop, -1, 2.0) };
    let js = serde_json::to_string(&stop).unwrap();
    assert_eq!(
        js,
        r#"{"kind":"stop","side":-1,"size":2.0,"price":100.0,"trail":null,"extreme":null,"weight":0.0,"stop":null}"#
    );
    let back: OrderResting = serde_json::from_str(&js).unwrap();
    assert_eq!(back, stop);
    assert_eq!(back.limit, None);

    let stop_limit = OrderResting { kind: OrderKind::StopLimit, limit: Some(99.0), ..stop };
    let sjs = serde_json::to_string(&stop_limit).unwrap();
    assert!(sjs.starts_with(r#"{"kind":"stop_limit","#), "{sjs}");
    assert!(sjs.ends_with(r#""stop":null,"limit":99.0}"#), "{sjs}");
    assert_eq!(serde_json::from_str::<OrderResting>(&sjs).unwrap(), stop_limit);
    assert_eq!(
        serde_json::to_string(&OrderKind::TakeProfit).unwrap(),
        r#""take_profit""#,
        "the stored word"
    );
}

/// The release compatibility pin: a working order with no release serializes to EXACTLY the bytes it
/// had before the field existed (pinned against a literal), an old body reads back with none, a
/// released order carries it LAST, and the three new kinds are their stored words.
#[test]
fn working_order_without_release_is_byte_identical() {
    let limit = OrderResting { price: Some(100.0), ..OrderResting::new(OrderKind::Limit, 1, 2.0) };
    let js = serde_json::to_string(&limit).unwrap();
    assert_eq!(
        js,
        r#"{"kind":"limit","side":1,"size":2.0,"price":100.0,"trail":null,"extreme":null,"weight":0.0,"stop":null}"#
    );
    let back: OrderResting = serde_json::from_str(&js).unwrap();
    assert_eq!((back.release, &back), (None, &limit));

    let held = OrderResting { release: Some(OrderRelease { at: 1_700, price: None }), ..limit };
    let hjs = serde_json::to_string(&held).unwrap();
    assert!(hjs.ends_with(r#""stop":null,"release":{"at":1700}}"#), "{hjs}");
    assert_eq!(serde_json::from_str::<OrderResting>(&hjs).unwrap(), held);
    for (kind, word) in [
        (OrderKind::MarketToLimit, "market_to_limit"),
        (OrderKind::AuctionOpen, "auction_open"),
        (OrderKind::AuctionClose, "auction_close"),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{word}\""), "{kind:?}");
    }
}

/// The trailing compatibility pin: a trailing order with no activation and no offset serializes to
/// EXACTLY the bytes it had before the fields existed (pinned against a literal), an old body reads
/// back with neither, a set one rides LAST, in declaration order, and the new kind is its stored word.
#[test]
fn working_order_without_activation_or_offset_is_byte_identical() {
    let trailing = OrderResting {
        trail: Some(2.0),
        extreme: Some(100.0),
        ..OrderResting::new(OrderKind::Trailing, -1, 1.0)
    };
    let js = serde_json::to_string(&trailing).unwrap();
    assert_eq!(
        js,
        r#"{"kind":"trailing","side":-1,"size":1.0,"price":null,"trail":2.0,"extreme":100.0,"weight":0.0,"stop":null}"#
    );
    let back: OrderResting = serde_json::from_str(&js).unwrap();
    assert_eq!((back.activation, back.limit_offset, &back), (None, None, &trailing));

    let limited = OrderResting {
        kind: OrderKind::TrailingLimit,
        activation: Some(105.0),
        limit_offset: Some(0.5),
        ..trailing
    };
    let ljs = serde_json::to_string(&limited).unwrap();
    assert!(ljs.starts_with(r#"{"kind":"trailing_limit","#), "{ljs}");
    assert!(ljs.ends_with(r#""stop":null,"activation":105.0,"limit_offset":0.5}"#), "{ljs}");
    assert_eq!(serde_json::from_str::<OrderResting>(&ljs).unwrap(), limited);
    assert!(limited.is_limit_bounded() && !limited.books_as_maker());
    assert_eq!(limited.trailing_limit_at(98.0), 97.5, "a sell's limit sits below its stop");
    let buy = OrderResting { side: 1, ..limited };
    assert_eq!(buy.trailing_limit_at(98.0), 98.5, "a buy's above it");
}

/// A held order is held strictly BEFORE its release instant and live from it on; an order with no
/// release is never held.
#[test]
fn an_order_is_held_until_its_release_and_live_from_it() {
    let none = |_: &str| None;
    let mut o = OrderResting::new(OrderKind::Market, 1, 1.0);
    assert!(!o.is_held(i64::MIN, none), "no release: never held");
    o.release = Some(OrderRelease { at: 1_000, price: None });
    assert!(o.is_held(999, none));
    assert!(!o.is_held(1_000, none), "live at the instant itself");
    assert!(!o.is_held(1_001, none));
    assert!(OrderRelease { at: 1_000, price: None }.holds(1_000, none));
    assert!(!OrderRelease { at: 1_000, price: None }.holds(999, none));
}

/// A CONDITIONAL release (time AND price): it holds only once the time has come AND its symbol's price
/// stands at or beyond the level on its side; an unknown or non-finite price releases nothing; the price
/// is not even asked before the time; and a time-only release ignores the lookup.
#[test]
fn a_conditional_release_holds_on_time_and_price_together() {
    let cond = |side| OrderReleasePrice {
        symbol: ustr::Ustr::from("BTCUSDT"),
        level: 100.0,
        side,
        venue: None,
    };
    let up = OrderRelease { at: 1_000, price: Some(cond(OrderReleasePriceSide::Above)) };
    let down = OrderRelease { at: 1_000, price: Some(cond(OrderReleasePriceSide::Below)) };
    let px = |p: f64| move |s: &str| (s == "BTCUSDT").then_some(p);
    assert!(up.holds(1_000, px(100.0)), "the level itself meets it");
    assert!(up.holds(1_000, px(101.0)) && !up.holds(1_000, px(99.9)));
    assert!(down.holds(1_000, px(100.0)) && down.holds(1_000, px(99.0)));
    assert!(!down.holds(1_000, px(100.1)));
    assert!(!up.holds(1_000, |_| None), "no price known: not released");
    assert!(!up.holds(1_000, px(f64::NAN)), "a price that is no number meets nothing");
    assert!(
        !up.holds(999, |_| panic!("the price is not asked before the time")),
        "the time alone holds it back"
    );
    let plain = OrderRelease { at: 1_000, price: None };
    assert!(plain.holds(1_000, |_| None), "a time-only release needs no price");

    // The wire shape: a time-only release keeps its bytes, a conditional one names its condition.
    assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"at":1000}"#);
    let js = serde_json::to_string(&up).unwrap();
    assert_eq!(js, r#"{"at":1000,"price":{"symbol":"BTCUSDT","level":100.0,"side":"above"}}"#);
    assert_eq!(serde_json::from_str::<OrderRelease>(&js).unwrap(), up);
    assert!(
        serde_json::from_str::<OrderReleasePrice>(r#"{"symbol":"X","level":1.0,"side":"up"}"#)
            .is_err(),
        "a side outside the two words is refused"
    );
}

/// Both trigger kinds reach their own working kind with the trigger as `price` and the limit as
/// `limit`; before this arm they fell through to a MARKET order. Every older arm is unchanged.
#[test]
fn a_stop_limit_and_a_take_profit_rest_as_their_own_kinds() {
    let req = |order_type, price| OrderRequest {
        side: 1,
        qty: 2.0,
        order_type,
        price,
        trigger_price: Some(101.0),
        weight: 0.5,
        ..Default::default()
    };
    for (order_type, price, kind) in [
        (OrderType::StopLimit, Some(100.0), OrderKind::StopLimit),
        (OrderType::TakeProfit, Some(100.0), OrderKind::TakeProfit),
        (OrderType::TakeProfit, None, OrderKind::TakeProfit),
    ] {
        let o = OrderResting::from_request(&req(order_type.clone(), price));
        assert_eq!(o.kind, kind, "{order_type}");
        assert_eq!((o.price, o.limit), (Some(101.0), price), "{order_type}");
        assert_eq!((o.side, o.size, o.weight, o.stop), (1, 2.0, 0.5, None), "{order_type}");
    }
    let stop = OrderResting::from_request(&req(OrderType::Stop, Some(100.0)));
    assert_eq!((stop.kind, stop.price, stop.limit), (OrderKind::Stop, Some(101.0), None));
    let market = OrderResting::from_request(&req(OrderType::Market, None));
    assert_eq!((market.kind, market.price, market.limit), (OrderKind::Market, None, None));
    let limit = OrderResting::from_request(&req(OrderType::Limit, Some(100.0)));
    assert_eq!((limit.kind, limit.price, limit.limit), (OrderKind::Limit, Some(100.0), None));
}

/// The wave-2 arms: an auction lifetime on a market or a limit order is its auction kind (a market
/// one unpriced whatever `price` it carries), a market-to-limit is its own kind, each carries the
/// protective `stop` the plain entries carry, and EVERY kind carries the request's release. An
/// auction lifetime on any other kind does not change it (the doors refuse that pair).
#[test]
fn the_auction_and_market_to_limit_entries_rest_as_their_own_kinds_and_every_kind_carries_its_release()
 {
    let release = Some(OrderRelease { at: 5_000, price: None });
    let req = |order_type, price, tif| OrderRequest {
        side: -1,
        qty: 3.0,
        order_type,
        price,
        trigger_price: Some(101.0),
        time_in_force: tif,
        weight: 0.5,
        stop: Some(110.0),
        release,
        ..Default::default()
    };
    use OrderKind::{AuctionClose, AuctionOpen, Limit, Market, MarketToLimit, Stop, StopLimit};
    use TimeInForce::{AtClose, AtOpen, Gtc};
    #[rustfmt::skip]
    let rows = [
        // (order_type, price, tif) → (kind, price, stop)
        (OrderType::Market, Some(99.0), AtOpen, (AuctionOpen, None, Some(110.0))),
        (OrderType::Limit, Some(99.0), AtOpen, (AuctionOpen, Some(99.0), Some(110.0))),
        (OrderType::Market, None, AtClose, (AuctionClose, None, Some(110.0))),
        (OrderType::Limit, Some(99.0), AtClose, (AuctionClose, Some(99.0), Some(110.0))),
        (OrderType::MarketToLimit, None, Gtc, (MarketToLimit, None, Some(110.0))),
        (OrderType::Stop, None, AtOpen, (Stop, Some(101.0), None)),
        (OrderType::StopLimit, Some(99.0), AtClose, (StopLimit, Some(101.0), None)),
        (OrderType::Limit, Some(99.0), Gtc, (Limit, Some(99.0), Some(110.0))),
        (OrderType::Market, None, Gtc, (Market, None, Some(110.0))),
    ];
    for (order_type, price, tif, want) in rows {
        let o = OrderResting::from_request(&req(order_type.clone(), price, tif));
        assert_eq!((o.kind, o.price, o.stop), want, "{order_type} {tif:?}");
        assert_eq!((o.side, o.size, o.weight), (-1, 3.0, 0.5), "{order_type} {tif:?}");
        assert_eq!(o.release, release, "{order_type} {tif:?} carries its release");
    }
    let trailing = OrderRequest {
        trail: Some(2.0),
        extreme: Some(100.0),
        ..req(OrderType::Market, None, Gtc)
    };
    assert_eq!(OrderResting::from_request(&trailing).release, release, "the trailing arm too");
    let now = OrderResting::from_request(&OrderRequest {
        release: None,
        ..req(OrderType::Limit, Some(1.0), Gtc)
    });
    assert_eq!(now.release, None);
}

/// The trailing kinds rest as their own kinds, the trigger price their activation and a
/// trailing_limit's offset carried; a `trail` on any other request keeps its backtest-only lowering
/// to a trailing stop with NO activation (its trigger price unread) and no offset, byte for byte.
#[test]
fn the_trailing_kinds_rest_as_their_own_kinds_with_their_activation() {
    let req = |order_type, trigger_price| OrderRequest {
        side: -1,
        qty: 2.0,
        order_type,
        trigger_price,
        trail: Some(3.0),
        extreme: Some(110.0),
        limit_offset: Some(0.5),
        weight: 0.5,
        ..Default::default()
    };
    use OrderKind::{Trailing, TrailingLimit};
    #[rustfmt::skip]
    let rows = [
        // (order_type, trigger price) → (kind, activation, limit offset)
        (OrderType::Trailing, Some(115.0), (Trailing, Some(115.0), None)),
        (OrderType::Trailing, None, (Trailing, None, None)),
        (OrderType::TrailingLimit, Some(115.0), (TrailingLimit, Some(115.0), Some(0.5))),
        (OrderType::TrailingLimit, None, (TrailingLimit, None, Some(0.5))),
        (OrderType::Market, Some(115.0), (Trailing, None, None)),
        (OrderType::Stop, Some(115.0), (Trailing, None, None)),
    ];
    for (order_type, trigger, want) in rows {
        let o = OrderResting::from_request(&req(order_type.clone(), trigger));
        assert_eq!((o.kind, o.activation, o.limit_offset), want, "{order_type} {trigger:?}");
        assert_eq!((o.price, o.trail, o.extreme), (None, Some(3.0), Some(110.0)), "{order_type}");
        assert_eq!((o.side, o.size, o.weight, o.limit), (-1, 2.0, 0.5, None), "{order_type}");
    }
}

/// The order-terms rule refuses ONLY a trigger kind with no trigger above zero, a stop-limit with no
/// limit above zero, a trigger kind that states a limit of zero or below, a market-to-limit that
/// states a limit at all, and a number that is not finite. A market order that carries a trigger is left alone (`fixtures/r5/risk.json` sizes one),
/// so is a plain limit at zero or below (a combo net can be negative), and so is a word this
/// platform does not know.
#[test]
fn the_order_terms_rule_refuses_only_a_missing_trigger_or_limit_and_a_non_finite_number() {
    let unknown = OrderType::Unrecognized("stop_market".into());
    let rows = [
        (OrderType::Market, None, None, None),
        (OrderType::Market, None, Some(65.6), None),
        (OrderType::Limit, Some(100.0), Some(90.0), None),
        (OrderType::Limit, None, None, None),
        (OrderType::Stop, None, Some(95.0), None),
        (OrderType::Stop, None, None, Some("a stop order needs a trigger price above zero")),
        (OrderType::Stop, None, Some(0.0), Some("needs a trigger price above zero")),
        (OrderType::Stop, None, Some(-1.0), Some("needs a trigger price above zero")),
        (OrderType::TakeProfit, None, Some(105.0), None),
        (OrderType::TakeProfit, Some(104.0), Some(105.0), None),
        (OrderType::TakeProfit, None, None, Some("a take_profit order needs a trigger price")),
        (OrderType::StopLimit, Some(99.0), Some(100.0), None),
        (OrderType::StopLimit, None, Some(100.0), Some("a stop_limit order needs a limit price")),
        (OrderType::StopLimit, Some(0.0), Some(100.0), Some("needs a limit price above zero")),
        (OrderType::StopLimit, Some(99.0), None, Some("needs a trigger price above zero")),
        (OrderType::TakeProfit, Some(0.0), Some(105.0), Some("the limit price 0 of a take_profit")),
        (OrderType::TakeProfit, Some(-1.0), Some(105.0), Some("limit price -1 of a take_profit")),
        (OrderType::Stop, Some(-1.0), Some(95.0), Some("the limit price -1 of a stop order")),
        (OrderType::Limit, Some(0.0), None, None),
        (OrderType::Limit, Some(-5.0), None, None),
        (OrderType::Limit, Some(f64::NAN), None, Some("the price NaN is not a finite number")),
        (OrderType::Market, None, Some(f64::INFINITY), Some("the trigger price inf is not")),
        (unknown, None, None, None),
        // a market-to-limit is an unpriced taker: its limit is its first fill's price
        (OrderType::MarketToLimit, None, None, None),
        (OrderType::MarketToLimit, None, Some(65.6), None),
        (
            OrderType::MarketToLimit,
            Some(100.0),
            None,
            Some("market_to_limit order takes no limit price"),
        ),
        (
            OrderType::MarketToLimit,
            Some(f64::NAN),
            None,
            Some("the price NaN is not a finite number"),
        ),
    ];
    for (order_type, price, trigger, refused) in rows {
        let got = order_terms_refusal(&order_type, price, trigger, None, None, None, None);
        match refused {
            None => assert_eq!(got, None, "{order_type} {price:?} {trigger:?}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{order_type} {price:?} {trigger:?}"));
                assert!(why.contains(words), "{order_type}: {why}");
            }
        }
        // a backtest-only `trail` on any of these kinds is never judged: the same verdict
        assert_eq!(
            order_terms_refusal(&order_type, price, trigger, Some(f64::NAN), None, None, None)
                .is_some(),
            refused.is_some(),
            "{order_type}: a trail is not read off the trailing kinds"
        );
    }
}

/// The trailing kinds' terms: a trail above zero, no price, an activation (the trigger price) above
/// zero when stated, and on a trailing_limit a limit offset of zero or more; a limit offset on any
/// other kind is refused. A caller with no trail to pass (`None`) refuses both kinds in these words.
#[test]
fn the_order_terms_rule_judges_a_trailing_kinds_trail_activation_and_offset() {
    let (tr, tl) = (OrderType::Trailing, OrderType::TrailingLimit);
    #[rustfmt::skip]
    let rows = [
        // (kind, price, trigger, trail, offset) → refused containing
        (tr.clone(), None, None, Some(2.0), None, None),
        (tr.clone(), None, Some(105.0), Some(2.0), None, None),
        (tl.clone(), None, None, Some(2.0), Some(0.0), None),
        (tl.clone(), None, Some(105.0), Some(2.0), Some(0.5), None),
        (tr.clone(), None, None, None, None, Some("a trailing order needs a trail distance above zero")),
        (tl.clone(), None, None, None, Some(0.5), Some("a trailing_limit order needs a trail distance above zero")),
        (tr.clone(), None, None, Some(0.0), None, Some("needs a trail distance above zero")),
        (tr.clone(), None, None, Some(-1.0), None, Some("needs a trail distance above zero")),
        (tr.clone(), None, None, Some(f64::INFINITY), None, Some("the trail inf is not a finite number")),
        (tr.clone(), Some(100.0), None, Some(2.0), None, Some("a trailing order takes no price")),
        (tl.clone(), Some(100.0), None, Some(2.0), Some(0.5), Some("a trailing_limit order takes no price")),
        (tr.clone(), None, Some(0.0), Some(2.0), None, Some("the activation price 0 of a trailing order")),
        (tl.clone(), None, Some(-1.0), Some(2.0), Some(0.5), Some("activation price -1 of a trailing_limit")),
        (tr.clone(), None, Some(f64::NAN), Some(2.0), None, Some("the trigger price NaN is not a finite")),
        (tl.clone(), None, None, Some(2.0), None, Some("needs a limit offset of zero or more")),
        (tl.clone(), None, None, Some(2.0), Some(-0.5), Some("the limit offset -0.5 of a trailing_limit")),
        (tl.clone(), None, None, Some(2.0), Some(f64::NAN), Some("the limit offset NaN is not a finite")),
        (tr.clone(), None, None, Some(2.0), Some(0.5), Some("this trailing order states 0.5")),
        (OrderType::Limit, Some(100.0), None, None, Some(0.5), Some("this limit order states 0.5")),
        (OrderType::StopLimit, Some(99.0), Some(100.0), None, Some(0.5), Some("belongs to a trailing_limit")),
    ];
    for (kind, price, trigger, trail, offset, refused) in rows {
        let got = order_terms_refusal(&kind, price, trigger, trail, offset, None, None);
        let req = OrderRequest {
            order_type: kind.clone(),
            price,
            trigger_price: trigger,
            trail,
            limit_offset: offset,
            ..Default::default()
        };
        assert_eq!(req.terms_refusal(), got, "the request's own call is the same rule");
        let case = format!("{kind} {price:?} {trigger:?} {trail:?} {offset:?}");
        match refused {
            None => assert_eq!(got, None, "{case}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{case}"));
                assert!(why.contains(words), "{case}: {why}");
            }
        }
    }
}

/// `OrderRequest::terms_refusal` hands the rule the request's peg and trail percent too, not only
/// the prices the two tables above vary.
#[test]
fn the_requests_own_terms_call_reads_its_peg_and_its_trail_percent() {
    let peg = OrderPeg { anchor: OrderPegAnchor::Mid, offset: 0.0 };
    let pegged = OrderRequest { order_type: OrderType::Pegged, ..Default::default() };
    let with_peg = OrderRequest { peg: Some(peg), ..pegged.clone() };
    let percent_on_a_limit = OrderRequest {
        order_type: OrderType::Limit,
        price: Some(100.0),
        trail_percent: Some(1.0),
        ..Default::default()
    };
    assert!(pegged.terms_refusal().is_some_and(|why| why.contains("needs its peg")));
    assert_eq!(with_peg.terms_refusal(), None);
    assert!(percent_on_a_limit.terms_refusal().is_some_and(|why| why.contains("trail percent")));
}

/// The attributes wave's terms: a trailing kind trails by exactly one distance, a price or a percent
/// above 0 and below 100 (a percent on any other kind is refused); a trailing take-profit takes an
/// optional offset of zero or more, no price and, in this wave, no activation.
#[test]
fn the_order_terms_rule_judges_a_percent_trail_and_a_trailing_take_profit() {
    let (tr, ttp) = (OrderType::Trailing, OrderType::TrailingTakeProfit);
    #[rustfmt::skip]
    let rows = [
        // (kind, price, trigger, trail, offset, percent) → refused containing
        (tr.clone(), None, None, None, None, Some(1.5), None),
        (OrderType::TrailingLimit, None, Some(105.0), None, Some(0.5), Some(99.9), None),
        (tr.clone(), None, None, Some(2.0), None, Some(1.5), Some("by a distance or by a percent, not both")),
        (tr.clone(), None, None, None, None, Some(0.0), Some("the trail percent 0 of a trailing order is not above 0 and below 100")),
        (tr.clone(), None, None, None, None, Some(100.0), Some("not above 0 and below 100")),
        (tr.clone(), None, None, None, None, Some(f64::NAN), Some("the trail percent NaN is not a finite number")),
        (OrderType::Stop, None, Some(95.0), None, None, Some(1.5), Some("a trail percent belongs to a trailing order; this stop order")),
        (ttp.clone(), None, None, Some(2.0), None, None, None),
        (ttp.clone(), None, None, Some(2.0), Some(0.0), None, None),
        (ttp.clone(), None, None, None, Some(0.5), Some(1.0), None),
        (ttp.clone(), None, None, None, None, None, Some("a trailing_take_profit order needs a trail distance above zero")),
        (ttp.clone(), Some(100.0), None, Some(2.0), None, None, Some("a trailing_take_profit order takes no price (its trigger trails")),
        (ttp.clone(), None, Some(105.0), Some(2.0), None, None, Some("takes no activation price in this build")),
        (ttp.clone(), None, None, Some(2.0), Some(-0.5), None, Some("the limit offset -0.5 of a trailing_take_profit")),
        (OrderType::Limit, Some(100.0), None, None, Some(0.5), None, Some("(or a trailing_take_profit one)")),
    ];
    for (kind, price, trigger, trail, offset, percent, refused) in rows {
        let got = order_terms_refusal(&kind, price, trigger, trail, offset, None, percent);
        let case = format!("{kind} {price:?} {trigger:?} {trail:?} {offset:?} {percent:?}");
        match refused {
            None => assert_eq!(got, None, "{case}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{case}"));
                assert!(why.contains(words), "{case}: {why}");
            }
        }
    }
}

/// The attributes rule: any attribute belongs to a limit order outside an auction; all-or-none takes
/// no `ioc`/`fok`, sweep-to-fill and a discretion no post-only (both take liquidity), and a
/// discretion is a finite number above zero.
#[test]
fn the_attributes_rule_refuses_an_attribute_off_a_limit_and_its_impossible_lifetimes() {
    use TimeInForce::{AtOpen, Day, Fok, Gtc, Ioc, PostOnly};
    let aon = OrderAttributes { all_or_none: true, ..Default::default() };
    let sweep = OrderAttributes { sweep_to_fill: true, ..Default::default() };
    let disc = |d| OrderAttributes { discretionary_amount: Some(d), ..Default::default() };
    #[rustfmt::skip]
    let rows = [
        (OrderType::Limit, Gtc, Some(aon), None),
        (OrderType::Limit, Day, Some(sweep), None),
        (OrderType::Limit, PostOnly, Some(aon), None),
        (OrderType::Limit, Gtc, Some(disc(0.25)), None),
        (OrderType::Limit, Gtc, None, None),
        (OrderType::Market, Gtc, None, None),
        (OrderType::Market, Gtc, Some(aon), Some("belongs to a limit order; this is a market order")),
        (OrderType::Stop, Gtc, Some(disc(1.0)), Some("this is a stop order")),
        (OrderType::Limit, Ioc, Some(aon), Some("takes no time in force `ioc` (all-or-none at once is `fok`)")),
        (OrderType::Limit, Fok, Some(aon), Some("takes no time in force `fok`")),
        (OrderType::Limit, PostOnly, Some(sweep), Some("a sweep-to-fill order takes liquidity at once")),
        (OrderType::Limit, PostOnly, Some(disc(0.25)), Some("a discretionary order takes liquidity inside its discretion, so it takes no time in force `post-only`")),
        (OrderType::Limit, Day, Some(disc(0.25)), None),
        (OrderType::Limit, AtOpen, Some(disc(1.0)), Some("takes no auction lifetime (`at-open`)")),
        (OrderType::Limit, Gtc, Some(disc(0.0)), Some("the discretionary amount 0 is not a finite number above zero")),
        (OrderType::Limit, Gtc, Some(disc(f64::NAN)), Some("the discretionary amount NaN")),
    ];
    for (kind, tif, attributes, refused) in rows {
        let got = attributes_refusal(&kind, tif, attributes.as_ref());
        let case = format!("{kind} {tif:?} {attributes:?}");
        match refused {
            None => assert_eq!(got, None, "{case}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{case}"));
                assert!(why.contains(words), "{case}: {why}");
            }
        }
    }
}

/// The adjustment rule: a stop or stop-limit only; its level above zero and on the PROTECTED side (a
/// sell stop's above its stop, a buy stop's below); the target's stop on the NEAR side of the level and
/// its terms the terms rule's (a stop-limit's limit, exactly one trailing distance).
#[test]
fn the_adjustment_rule_judges_its_level_side_and_target() {
    let at = |at, to| Some(OrderStopAdjust { at, to });
    let stop = |s| OrderStopAdjustTo::Stop { stop: s };
    let trail = |t, p| OrderStopAdjustTo::Trailing { stop: 100.0, trail: t, trail_percent: p };
    #[rustfmt::skip]
    let rows = [
        // (kind, side, stop, adjust) → refused containing
        (OrderType::Stop, -1, Some(95.0), at(105.0, stop(100.0)), None),
        (OrderType::StopLimit, -1, Some(95.0), at(105.0, OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.5 }), None),
        (OrderType::Stop, -1, Some(95.0), at(105.0, trail(Some(3.0), None)), None),
        (OrderType::Stop, -1, Some(95.0), at(105.0, trail(None, Some(2.0))), None),
        (OrderType::Stop, 1, Some(105.0), at(95.0, stop(100.0)), None),
        (OrderType::Stop, -1, Some(95.0), None, None),
        (OrderType::Limit, -1, None, at(105.0, stop(100.0)), Some("belongs to a stop or a stop_limit order; this is a limit order")),
        (OrderType::Stop, -1, Some(95.0), at(90.0, stop(85.0)), Some("not on the protected side of the stop 95")),
        (OrderType::Stop, 1, Some(105.0), at(110.0, stop(108.0)), Some("not on the protected side")),
        (OrderType::Stop, -1, Some(95.0), at(f64::NAN, stop(100.0)), Some("the adjustment level NaN")),
        (OrderType::Stop, -1, Some(95.0), at(105.0, stop(106.0)), Some("not on the near side of its level 105")),
        (OrderType::Stop, 1, Some(105.0), at(95.0, stop(94.0)), Some("not on the near side")),
        (OrderType::Stop, -1, Some(95.0), at(105.0, stop(0.0)), Some("target stop 0 is not a finite number above zero")),
        (OrderType::Stop, -1, Some(95.0), at(105.0, OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 0.0 }), Some("the adjustment's target: a stop_limit order needs a limit price above zero")),
        (OrderType::Stop, -1, Some(95.0), at(105.0, trail(None, None)), Some("needs a trail distance above zero")),
        (OrderType::Stop, -1, Some(95.0), at(105.0, trail(Some(3.0), Some(2.0))), Some("not both")),
    ];
    for (kind, side, trigger, adjust, refused) in rows {
        let got = stop_adjust_refusal(&kind, side, trigger, adjust.as_ref());
        let case = format!("{kind} {side} {trigger:?} {adjust:?}");
        match refused {
            None => assert_eq!(got, None, "{case}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{case}"));
                assert!(why.contains(words), "{case}: {why}");
            }
        }
    }
}

/// The price-source rule: a roster id exactly, never an alias or another case.
#[test]
fn a_price_source_is_a_roster_venue_exactly() {
    assert_eq!(price_source_refusal("binance"), None);
    assert_eq!(price_source_refusal("ibkr"), None);
    for bad in ["Binance", "binance-perp", "", "nyse"] {
        let why = price_source_refusal(bad).expect(bad);
        assert!(why.contains(&format!("`{bad}` is not a venue this platform knows")), "{why}");
        assert!(why.contains("binance, bybit"), "{why}");
    }
}

/// The attributes wave's arms of `OrderResting::from_request`: a trailing take-profit rests as its own
/// kind with its trail or percent and its offset (no activation is read: the rule refuses one), an
/// at-open market-to-limit is the opening auction's, a limit carries all-or-none and its discretion,
/// a stop or stop-limit its adjustment, and a percent only where a kind trails.
#[test]
fn the_attributes_waves_requests_rest_with_their_terms() {
    let ttp = OrderRequest {
        side: -1,
        qty: 1.0,
        order_type: OrderType::TrailingTakeProfit,
        trail_percent: Some(1.5),
        limit_offset: Some(0.25),
        extreme: Some(90.0),
        ..Default::default()
    };
    let o = OrderResting::from_request(&ttp);
    assert_eq!(
        (o.kind, o.trail, o.trail_percent),
        (OrderKind::TrailingTakeProfit, None, Some(1.5))
    );
    assert_eq!((o.limit_offset, o.extreme, o.activation), (Some(0.25), Some(90.0), None));
    assert!(o.is_limit_bounded());

    let mtl = |tif| OrderRequest {
        order_type: OrderType::MarketToLimit,
        time_in_force: tif,
        qty: 1.0,
        ..Default::default()
    };
    assert_eq!(
        OrderResting::from_request(&mtl(TimeInForce::AtOpen)).kind,
        OrderKind::AuctionOpenToLimit
    );
    assert_eq!(OrderResting::from_request(&mtl(TimeInForce::Gtc)).kind, OrderKind::MarketToLimit);

    let attrs =
        OrderAttributes { all_or_none: true, sweep_to_fill: true, discretionary_amount: Some(0.5) };
    let limit = OrderRequest {
        order_type: OrderType::Limit,
        price: Some(100.0),
        qty: 1.0,
        attributes: Some(attrs),
        trail_percent: Some(1.0),
        ..Default::default()
    };
    let l = OrderResting::from_request(&limit);
    assert_eq!(
        (l.kind, l.all_or_none, l.discretionary_amount),
        (OrderKind::Limit, true, Some(0.5))
    );
    assert_eq!(l.trail_percent, None, "a percent belongs to a trailing kind only");

    let adjust = OrderStopAdjust { at: 105.0, to: OrderStopAdjustTo::Stop { stop: 100.0 } };
    for (kind, carried) in [
        (OrderType::Stop, true),
        (OrderType::StopLimit, true),
        (OrderType::TakeProfit, false),
        (OrderType::Limit, false),
    ] {
        let req = OrderRequest {
            order_type: kind.clone(),
            trigger_price: Some(95.0),
            price: Some(94.0),
            qty: 1.0,
            adjust: Some(adjust),
            ..Default::default()
        };
        let got = OrderResting::from_request(&req).adjust.map(|a| *a);
        assert_eq!(got, carried.then_some(adjust), "{kind}");
    }
}

/// A pegged order's terms: it carries its peg (any anchor, any finite offset, either sign) and states
/// no price and no trigger; a peg on any other kind is refused, and so is a pegged order a caller with
/// no peg to pass hands over.
#[test]
fn the_order_terms_rule_judges_a_pegged_orders_peg_and_refuses_a_peg_elsewhere() {
    let pegged = OrderType::Pegged;
    let mid = OrderPeg { anchor: OrderPegAnchor::Mid, offset: -0.05 };
    let nan = OrderPeg { offset: f64::NAN, ..mid };
    #[rustfmt::skip]
    let rows = [
        // (kind, price, trigger, peg) → refused containing
        (pegged.clone(), None, None, Some(mid), None),
        (pegged.clone(), None, None, Some(OrderPeg { anchor: OrderPegAnchor::Primary, offset: 0.0 }), None),
        (pegged.clone(), None, None, None, Some("a pegged order needs its peg: an anchor (best, mid, market, primary)")),
        (pegged.clone(), Some(100.0), None, Some(mid), Some("a pegged order takes no price")),
        (pegged.clone(), None, Some(100.0), Some(mid), Some("a pegged order takes no trigger price")),
        (pegged.clone(), None, None, Some(nan), Some("the peg offset NaN is not a finite number")),
        (OrderType::Limit, Some(100.0), None, Some(mid), Some("a peg belongs to a pegged order; this limit order")),
        (OrderType::Market, None, None, Some(mid), Some("states one (to the mid price)")),
    ];
    for (kind, price, trigger, peg, refused) in rows {
        let got = order_terms_refusal(&kind, price, trigger, None, None, peg.as_ref(), None);
        let case = format!("{kind} {price:?} {trigger:?} {peg:?}");
        match refused {
            None => assert_eq!(got, None, "{case}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{case}"));
                assert!(why.contains(words), "{case}: {why}");
            }
        }
    }
}

/// The anchors: each prices its side of the touch (`Primary` as `Best`, `Market` on the far side), the
/// offset is aggressive when positive, and the four words are serde's, exactly.
#[test]
fn the_peg_anchors_price_their_side_and_their_words_are_serdes() {
    let (bid, ask) = (100.0, 102.0);
    assert_eq!(
        OrderPegAnchor::Market.price(1, bid, ask, 0.5),
        Some(102.5),
        "a buy at the ask, plus"
    );
    assert_eq!(
        OrderPegAnchor::Market.price(-1, bid, ask, -0.5),
        Some(100.5),
        "a sell at the bid, passive"
    );
    assert_eq!(
        OrderPegAnchor::Primary.price(1, bid, ask, 0.5),
        OrderPegAnchor::Best.price(1, bid, ask, 0.5)
    );
    assert_eq!(OrderPegAnchor::Primary.price(-1, bid, ask, 0.0), Some(102.0));
    for anchor in OrderPegAnchor::ALL {
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, format!("\"{}\"", anchor.as_word()));
        assert_eq!(OrderPegAnchor::parse(anchor.as_word()), Some(anchor));
        assert_eq!(serde_json::from_str::<OrderPegAnchor>(&json).unwrap(), anchor);
    }
    assert_eq!(OrderPegAnchor::parse("Market"), None, "exact words only");
    assert_eq!(OrderPegAnchor::accepted_words(), "best, mid, market, primary");
}

/// A request with no peg is the bytes it always was; a pegged one round-trips with its peg, and a peg
/// carrying a term this build does not know is refused rather than read without it.
#[test]
fn order_request_without_peg_is_byte_identical() {
    let plain = OrderRequest { order_client_id: "c".into(), qty: 1.0, ..Default::default() };
    assert!(!serde_json::to_string(&plain).unwrap().contains("peg"));
    let pegged = OrderRequest {
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.01 }),
        ..plain
    };
    let json = serde_json::to_string(&pegged).unwrap();
    assert!(json.contains(r#""order_type":"pegged""#), "{json}");
    assert!(json.contains(r#""peg":{"anchor":"best","offset":0.01}"#), "{json}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&json).unwrap(), pegged);
    let later = json.replace(r#""offset":0.01}"#, r#""offset":0.01,"cap":5.0}"#);
    assert!(serde_json::from_str::<OrderRequest>(&later).is_err(), "{later}");
}

/// The display rule refuses ONLY a display on a non-limit order, one not above zero, and one at least the
/// order's quantity; a display named nowhere is no refusal on any kind.
#[test]
fn the_display_rule_refuses_a_display_off_a_limit_at_or_below_zero_or_not_below_the_quantity() {
    let rows = [
        (OrderType::Limit, 10.0, None, None),
        (OrderType::Market, 10.0, None, None),
        (OrderType::Limit, 10.0, Some(1.0), None),
        (OrderType::Limit, 10.0, Some(9.999), None),
        (OrderType::Limit, 10.0, Some(10.0), Some("not below the order's quantity 10")),
        (OrderType::Limit, 10.0, Some(11.0), Some("it hides nothing")),
        (OrderType::Limit, 10.0, Some(f64::INFINITY), Some("not below the order's quantity")),
        // a display of zero is a HIDDEN order (the venue's `hidden` attribute judges it)
        (OrderType::Limit, 10.0, Some(0.0), None),
        (OrderType::Market, 10.0, Some(0.0), Some("belongs to a limit order")),
        (OrderType::Limit, 10.0, Some(-1.0), Some("the display quantity -1 is not zero or more")),
        (OrderType::Limit, 10.0, Some(f64::NAN), Some("NaN is not zero or more")),
        (OrderType::Market, 10.0, Some(1.0), Some("belongs to a limit order; this is a market")),
        (OrderType::StopLimit, 10.0, Some(1.0), Some("this is a stop_limit order")),
        (OrderType::MarketToLimit, 10.0, Some(1.0), Some("this is a market_to_limit order")),
    ];
    for (order_type, qty, shown, refused) in rows {
        let got = display_refusal(&order_type, qty, shown);
        match refused {
            None => assert_eq!(got, None, "{order_type} {qty} {shown:?}"),
            Some(words) => {
                let why = got.unwrap_or_else(|| panic!("{order_type} {qty} {shown:?}"));
                assert!(why.contains(words), "{order_type}: {why}");
            }
        }
    }
}

#[test]
fn time_in_force_defaults_gtc_and_serializes_screaming() {
    assert_eq!(TimeInForce::default(), TimeInForce::Gtc);
    assert_eq!(serde_json::to_string(&TimeInForce::Gtc).unwrap(), "\"GTC\"");
    assert_eq!(serde_json::to_string(&TimeInForce::Ioc).unwrap(), "\"IOC\"");
    assert_eq!(serde_json::to_string(&TimeInForce::Day).unwrap(), "\"DAY\"");
}

/// `TimeInForce::ALL` is a hand list, and this is what keeps it whole: `index` has one arm per
/// variant and no wildcard, so a new variant stops this file compiling until it gets an arm, and
/// then `ARMS` (the arm count) and `ALL` must grow with it.
#[test]
fn time_in_force_all_is_every_variant_in_declaration_order() {
    const ARMS: usize = 8;
    fn index(k: TimeInForce) -> usize {
        match k {
            TimeInForce::Gtc => 0,
            TimeInForce::Ioc => 1,
            TimeInForce::Fok => 2,
            TimeInForce::Gtd => 3,
            TimeInForce::Day => 4,
            TimeInForce::PostOnly => 5,
            TimeInForce::AtOpen => 6,
            TimeInForce::AtClose => 7,
        }
    }
    assert_eq!(TimeInForce::ALL.len(), ARMS, "ALL and the arms of `idx` disagree");
    for (i, &k) in TimeInForce::ALL.iter().enumerate() {
        // `index(ALL[i]) == i` gives `ALL[index(k)] == k` AND refuses a duplicate standing in for
        // a missing variant; the discriminant pins declaration order without trusting `index`.
        assert_eq!(index(k), i, "{k:?} is not at its own index of ALL");
        assert_eq!(TimeInForce::ALL[index(k)], k);
        assert_eq!(k as usize, i, "{k:?}: ALL is not in declaration order");
    }
}

/// Post-only is a VARIANT, not a field: the request keeps its size, a default request still
/// serializes `GTC`, and the new lifetime is the one journal/wire spelling `POST_ONLY` (the
/// journal reads an older record, which never names it, as `GTC`).
#[test]
fn post_only_is_a_variant_that_adds_no_bytes_to_the_request() {
    assert_eq!(std::mem::size_of::<TimeInForce>(), 1, "a fieldless enum is one byte");
    assert_eq!(serde_json::to_string(&TimeInForce::PostOnly).unwrap(), "\"POST_ONLY\"");
    assert_eq!(
        serde_json::from_str::<TimeInForce>("\"POST_ONLY\"").unwrap(),
        TimeInForce::PostOnly
    );
    let plain = serde_json::to_string(&OrderRequest::default()).unwrap();
    assert!(plain.contains("\"time_in_force\":\"GTC\""), "the default is unchanged: {plain}");
    let post_only = OrderRequest { time_in_force: TimeInForce::PostOnly, ..Default::default() };
    let back: OrderRequest =
        serde_json::from_str(&serde_json::to_string(&post_only).unwrap()).unwrap();
    assert_eq!(back.time_in_force, TimeInForce::PostOnly);
    // an older record, written before the variant existed, still reads back as GTC
    let old = plain.replace("\"GTC\"", "\"IOC\"");
    let old: OrderRequest = serde_json::from_str(&old).unwrap();
    assert_eq!(old.time_in_force, TimeInForce::Ioc);
}

/// The two auction lifetimes are appended variants too: still one byte, the journal/wire spellings
/// `AT_OPEN`/`AT_CLOSE`, a default request unchanged. An older build has neither name, so a record
/// carrying one is a rollback door (as `POST_ONLY` was), and `is_auction` is exactly these two.
#[test]
fn the_auction_lifetimes_are_appended_variants_that_add_no_bytes_to_the_request() {
    assert_eq!(std::mem::size_of::<TimeInForce>(), 1);
    for (t, wire) in [(TimeInForce::AtOpen, "\"AT_OPEN\""), (TimeInForce::AtClose, "\"AT_CLOSE\"")]
    {
        assert_eq!(serde_json::to_string(&t).unwrap(), wire);
        assert_eq!(serde_json::from_str::<TimeInForce>(wire).unwrap(), t);
    }
    let plain = serde_json::to_string(&OrderRequest::default()).unwrap();
    assert!(plain.contains("\"time_in_force\":\"GTC\""), "{plain}");
    let auctions: Vec<TimeInForce> =
        TimeInForce::ALL.iter().copied().filter(|t| t.is_auction()).collect();
    assert_eq!(auctions, [TimeInForce::AtOpen, TimeInForce::AtClose]);
}

/// The operator door: the five lifetimes plus `post-only` and the two auctions, any case, an
/// underscore for the hyphen too; anything else is refused naming the accepted words (never the
/// default `Gtc`).
#[test]
fn the_time_in_force_door_reads_the_eight_words_and_refuses_the_rest() {
    for &t in TimeInForce::ALL {
        assert_eq!(TimeInForce::parse(t.as_word()), Ok(t), "{t:?}");
        assert_eq!(TimeInForce::parse(&t.as_word().to_uppercase()), Ok(t), "{t:?} upper");
    }
    assert_eq!(TimeInForce::parse("  Post_Only "), Ok(TimeInForce::PostOnly));
    assert_eq!(TimeInForce::parse("at_open"), Ok(TimeInForce::AtOpen));
    assert_eq!(TimeInForce::parse("AT-CLOSE"), Ok(TimeInForce::AtClose));
    assert_eq!(TimeInForce::parse("gtc"), Ok(TimeInForce::Gtc));
    assert_eq!(TimeInForce::parse("day "), Ok(TimeInForce::Day), "trimmed");
    for bad in ["", "good-till-cancel", "postonly", "gtx", "opg", "moc", "atopen"] {
        let e = TimeInForce::parse(bad).expect_err(bad);
        let text = e.to_string();
        assert!(text.contains("gtc, ioc, fok, gtd, day, post-only, at-open, at-close"), "{text}");
    }
}

/// The lifetime rule both doors apply: a non-default lifetime is for a limit order, `gtd` needs a
/// future expiry, and nothing else takes one. The default `gtc` is never refused, whatever the kind.
#[test]
fn the_lifetime_rule_refuses_what_an_adapter_would_drop() {
    let now = 1_000_000;
    let limit = OrderType::Limit;
    // the default is everywhere legal
    for ot in [OrderType::Market, OrderType::Limit, OrderType::Stop, OrderType::MarketToLimit] {
        assert_eq!(lifetime_refusal(&ot, TimeInForce::Gtc, None, None, now), None, "{ot}");
    }
    // every lifetime is legal on a limit order (gtd with a future expiry)
    for &tif in TimeInForce::ALL.iter().filter(|t| **t != TimeInForce::Gtd) {
        assert_eq!(lifetime_refusal(&limit, tif, None, None, now), None, "{tif:?}");
    }
    assert_eq!(lifetime_refusal(&limit, TimeInForce::Gtd, Some(now + 1), None, now), None);
    // ...and on nothing else: the word would be dropped and the order would run without it
    for ot in [OrderType::Market, OrderType::Stop, OrderType::StopLimit, OrderType::MarketToLimit] {
        let why = lifetime_refusal(&ot, TimeInForce::PostOnly, None, None, now).expect("refused");
        assert!(why.contains("post-only") && why.contains("limit order"), "{ot}: {why}");
    }
    // gtd needs an expiry, in the future; nothing else takes one
    assert!(
        lifetime_refusal(&limit, TimeInForce::Gtd, None, None, now)
            .unwrap()
            .contains("needs an expiry")
    );
    assert!(
        lifetime_refusal(&limit, TimeInForce::Gtd, Some(now), None, now)
            .unwrap()
            .contains("not in the future")
    );
    assert!(
        lifetime_refusal(&limit, TimeInForce::Day, Some(now + 5), None, now)
            .unwrap()
            .contains("belongs to time in force `gtd`")
    );
}

/// An auction lifetime belongs to a market OR a limit order (market-on-open, limit-on-close), `at-open`
/// to a market-to-limit too (the opening auction's market-to-limit), and to nothing else, and takes no
/// expiry: the auction is its deadline.
#[test]
fn the_lifetime_rule_admits_an_auction_on_a_market_or_a_limit_order_only() {
    let now = 1_000_000;
    let mtl = OrderType::MarketToLimit;
    assert_eq!(lifetime_refusal(&mtl, TimeInForce::AtOpen, None, None, now), None);
    let why = lifetime_refusal(&mtl, TimeInForce::AtClose, None, None, now).expect("at-close");
    assert!(why.contains("belongs to a market or a limit order; this"), "{why}");
    for tif in [TimeInForce::AtOpen, TimeInForce::AtClose] {
        for ot in [OrderType::Market, OrderType::Limit] {
            assert_eq!(lifetime_refusal(&ot, tif, None, None, now), None, "{ot} {tif:?}");
        }
        let unknown = OrderType::Unrecognized("moc".into());
        for ot in [
            OrderType::Stop,
            OrderType::StopLimit,
            OrderType::TakeProfit,
            OrderType::TrailingTakeProfit,
            unknown,
        ] {
            let why = lifetime_refusal(&ot, tif, None, None, now).expect("refused");
            assert!(why.contains("belongs to a market or a limit order"), "{ot} {tif:?}: {why}");
        }
        let why = lifetime_refusal(&OrderType::Market, tif, Some(now + 5), None, now).unwrap();
        assert!(why.contains("belongs to time in force `gtd`"), "{tif:?}: {why}");
    }
}

/// A release must be in the future, and a `gtd` order must outlive it; every other lifetime takes one
/// (a day order is anchored when it is released). An order with no release is judged as before.
#[test]
fn the_lifetime_rule_refuses_a_past_release_and_one_the_order_would_not_live_to_see() {
    let now = 1_000_000;
    let at = |t| Some(OrderRelease { at: t, price: None });
    let limit = OrderType::Limit;
    for &tif in TimeInForce::ALL.iter().filter(|t| **t != TimeInForce::Gtd) {
        assert_eq!(lifetime_refusal(&limit, tif, None, at(now + 1), now), None, "{tif:?}");
    }
    for ot in [OrderType::Market, OrderType::Stop, OrderType::StopLimit, OrderType::MarketToLimit] {
        assert_eq!(lifetime_refusal(&ot, TimeInForce::Gtc, None, at(now + 1), now), None, "{ot}");
    }
    let gtd =
        |expiry, release| lifetime_refusal(&limit, TimeInForce::Gtd, Some(expiry), release, now);
    assert_eq!(gtd(now + 10, at(now + 5)), None, "expires after its release");
    for (expiry, release) in [(now + 5, now + 5), (now + 5, now + 10)] {
        let why = gtd(expiry, at(release)).expect("refused");
        assert!(why.contains("would expire before it is released"), "{expiry} {release}: {why}");
    }
    // the instant itself, the past, and a modern clock written in SECONDS
    let wall = 1_700_000_000_000;
    for past in [wall, wall - 1, 1_700_000_000] {
        let why =
            lifetime_refusal(&limit, TimeInForce::Gtc, None, at(past), wall).expect("refused");
        assert!(why.contains("is not in the future"), "{past}: {why}");
    }
    // the lifetime's own refusal comes first
    let why = lifetime_refusal(&OrderType::Stop, TimeInForce::Ioc, None, at(now - 1), now).unwrap();
    assert!(why.contains("belongs to a limit order"), "{why}");
}

#[test]
fn gtc_ioc_fok_never_expire() {
    // deadline/anchor deliberately in the deep past — none of these ever expires. A post-only
    // order rests like GTC: it is judged once, at arrival. An auction order's deadline is its
    // auction, which the book that knows the session enforces.
    for tif in [
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::PostOnly,
        TimeInForce::AtOpen,
        TimeInForce::AtClose,
    ] {
        assert!(
            !time_in_force_expired(tif, Some(1), 0, 10 * MS_PER_DAY),
            "{tif:?} must never expire by this predicate"
        );
    }
}

#[test]
fn gtd_expires_inclusively_at_the_deadline() {
    // before, at, after — inclusive boundary at `now == deadline`.
    assert!(!time_in_force_expired(TimeInForce::Gtd, Some(2_000), 0, 1_999), "before the deadline");
    assert!(
        time_in_force_expired(TimeInForce::Gtd, Some(2_000), 0, 2_000),
        "AT the deadline (inclusive)"
    );
    assert!(time_in_force_expired(TimeInForce::Gtd, Some(2_000), 0, 2_001), "after the deadline");
}

#[test]
fn gtd_without_a_deadline_never_expires() {
    assert!(!time_in_force_expired(TimeInForce::Gtd, None, 0, i64::MAX / 2));
}

#[test]
fn day_expires_when_now_crosses_the_utc_day_boundary_from_its_anchor() {
    let anchor = 12 * 3_600_000; // mid-day on UTC day 0
    assert!(
        !time_in_force_expired(TimeInForce::Day, None, anchor, anchor),
        "the anchor moment is alive"
    );
    assert!(
        !time_in_force_expired(TimeInForce::Day, None, anchor, MS_PER_DAY - 1),
        "still alive through the end of the anchor's UTC day"
    );
    assert!(
        time_in_force_expired(TimeInForce::Day, None, anchor, MS_PER_DAY),
        "expired on the first ms of the next UTC day"
    );
}

#[test]
fn day_survives_its_whole_anchor_day_at_a_modern_clock() {
    // guards the seconds-vs-ms / born-expired class: a modern anchor lives out its own day.
    let today = 20_650 * MS_PER_DAY;
    assert!(
        !time_in_force_expired(TimeInForce::Day, None, today, today),
        "born on its day, not expired"
    );
    assert!(
        !time_in_force_expired(TimeInForce::Day, None, today, today + MS_PER_DAY - 1),
        "alive to the last ms of the anchor day"
    );
    assert!(
        time_in_force_expired(TimeInForce::Day, None, today, today + MS_PER_DAY),
        "expired the next UTC day"
    );
}

#[test]
fn day_boundary_holds_across_a_leap_day() {
    // 2024-02-29 is UTC day 19782; 2024-03-01 is 19783 — a Day order anchored on the leap day
    // survives it and expires the next calendar day, exactly like any other pair of days.
    let leap = 19_782 * MS_PER_DAY;
    assert_eq!(utc_day(leap + 23 * 3_600_000), 19_782, "23:00 on the leap day is the same day");
    assert!(!time_in_force_expired(TimeInForce::Day, None, leap, leap + 23 * 3_600_000));
    assert!(
        time_in_force_expired(TimeInForce::Day, None, leap, leap + MS_PER_DAY),
        "the day after"
    );
}

#[test]
fn order_request_time_in_force_is_additive() {
    // a serialized request WITHOUT time_in_force deserializes as GTC (back-compat)
    let r: OrderRequest = serde_json::from_str(
            r#"{"order_client_id":"c","venue":"v","symbol":"s","side":1,"qty":1.0,"order_type":"market"}"#,
        )
        .unwrap();
    assert_eq!(r.time_in_force, TimeInForce::Gtc);
    assert_eq!(r.gtd_expiry, None);
}

#[test]
fn default_literal_matches_serde() {
    // Piece-1 refactor guard: the live runtime builds OrderRequest via `..Default::default()`
    // instead of a `serde_json` round-trip. This pins that the two produce byte-identical
    // values — i.e. every field's Rust `Default` equals its `#[serde(default)]`. If a future
    // field's two defaults diverge, this fails before the hot-path construction can drift.
    let via_literal = OrderRequest {
        order_client_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(3.0),
        reduce_only: true,
        timestamp: 42,
        ..Default::default()
    };
    let via_serde: OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "c", "venue": "v", "symbol": "s",
        "side": 1, "qty": 2.0, "order_type": "limit", "price": 3.0,
        "reduce_only": true, "timestamp": 42,
    }))
    .unwrap();
    assert_eq!(via_literal, via_serde);
    // `order_type`'s Rust default is `Market` (it was the empty string while the field was a
    // `String`); serde has NO default for it — a payload without the field is refused, as before.
    assert_eq!(OrderRequest::default().order_type, OrderType::Market);
    let missing = serde_json::from_value::<OrderRequest>(serde_json::json!({
        "order_client_id": "c", "venue": "v", "symbol": "s", "side": 1, "qty": 2.0,
    }));
    assert!(missing.is_err(), "order_type stays a required field");
}

#[test]
fn build_bracket_wires_oto_oco_linkage() {
    let spec = OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1, // long entry
        qty: 3.0,
        entry_price: Some(100.0), // limit entry
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let [entry, stop_loss, take_profit] = build_bracket(&spec, "E", "S", "T");

    // shared order-list id = the entry order client id; the three orders all carry it
    assert_eq!(entry.order_list_id.as_deref(), Some("E"));
    assert_eq!(stop_loss.order_list_id.as_deref(), Some("E"));
    assert_eq!(take_profit.order_list_id.as_deref(), Some("E"));

    // entry: OTO parent, limit, long, links to both exits
    assert_eq!(entry.contingency_type, Some(ContingencyType::Oto));
    assert_eq!(entry.order_type, OrderType::Limit);
    assert_eq!(entry.side, 1);
    assert_eq!(entry.price, Some(100.0));
    assert_eq!(entry.parent_order_id, None);
    assert_eq!(entry.linked_order_ids, vec!["S".to_string(), "T".to_string()]);

    // stop-loss: OCO child of entry, opposite side, reduce-only, stop @ trigger, links to TP
    assert_eq!(stop_loss.contingency_type, Some(ContingencyType::Oco));
    assert_eq!(stop_loss.parent_order_id.as_deref(), Some("E"));
    assert_eq!(stop_loss.side, -1);
    assert!(stop_loss.reduce_only);
    assert_eq!(stop_loss.order_type, OrderType::Stop);
    assert_eq!(stop_loss.trigger_price, Some(95.0));
    assert_eq!(stop_loss.linked_order_ids, vec!["T".to_string()]);

    // take-profit: OCO child of entry, opposite side, reduce-only, limit @ tp, links to SL
    assert_eq!(take_profit.contingency_type, Some(ContingencyType::Oco));
    assert_eq!(take_profit.parent_order_id.as_deref(), Some("E"));
    assert_eq!(take_profit.side, -1);
    assert!(take_profit.reduce_only);
    assert_eq!(take_profit.order_type, OrderType::Limit);
    assert_eq!(take_profit.price, Some(110.0));
    assert_eq!(take_profit.linked_order_ids, vec!["S".to_string()]);
    // a spec that names no account builds three legs that name none
    assert!([&entry, &stop_loss, &take_profit].iter().all(|o| o.account.is_none()));
}

/// A bracket NAMES its account, and all three legs carry it: the entry is routed by it and the
/// held exits are released onto the engine it resolved to. `Some(Default)` is carried too — it is a
/// NAME, and routes differently from `None` on a venue that runs two engines.
#[test]
fn build_bracket_carries_the_account_onto_all_three_legs() {
    use crate::accounts::account_keys::AccountLabel;
    for account in [AccountLabel::parse("PERP").unwrap(), AccountLabel::Default] {
        let spec = OrderBracket {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 95.0,
            take_profit: 110.0,
            account: Some(account.clone()),
            order_client_id: None,
        };
        for leg in build_bracket(&spec, "E", "S", "T") {
            assert_eq!(leg.account, Some(account.clone()), "{}", leg.order_client_id);
        }
    }
}

/// The spec is journaled and sent between crates as JSON: an account-less spec is BYTE-IDENTICAL to the
/// pre-field one (no `account` key), a record written before the field reads back as `None`, and a
/// named spec round-trips, the unlabelled account included (its own `AccountLabel` serde refuses it,
/// which is why the field goes through `wire_account_option`).
#[test]
fn a_bracket_spec_serialises_its_account_only_when_named_and_reads_an_old_record() {
    use crate::accounts::account_keys::AccountLabel;
    let spec = |account| OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account,
        order_client_id: None,
    };
    let bare = serde_json::to_string(&spec(None)).unwrap();
    assert!(!bare.contains("account"), "{bare}");
    let old = r#"{"venue":"binance","symbol":"BTCUSDT.P","side":1,"qty":1.0,
                  "entry_price":100.0,"stop_loss":95.0,"take_profit":110.0}"#;
    assert_eq!(serde_json::from_str::<OrderBracket>(old).unwrap(), spec(None));
    for account in [AccountLabel::parse("PERP").unwrap(), AccountLabel::Default] {
        let named = spec(Some(account));
        let json = serde_json::to_string(&named).expect("a named spec serialises");
        assert_eq!(serde_json::from_str::<OrderBracket>(&json).unwrap(), named, "{json}");
    }
}

/// The type's words are the upper-case bytes the `String` field carried (`"OTO"`, `"OCO"`; the
/// whole-request pin is `bracket_leg_shaped_request_is_byte_identical_too`), `"OUO"` beside them, and
/// no other spelling reads.
#[test]
fn contingency_type_words_are_the_bytes_the_string_carried() {
    for (kind, word) in [
        (ContingencyType::Oto, r#""OTO""#),
        (ContingencyType::Oco, r#""OCO""#),
        (ContingencyType::Ouo, r#""OUO""#),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), word);
        assert_eq!(serde_json::from_str::<ContingencyType>(word).unwrap(), kind);
    }
    assert!(serde_json::from_str::<ContingencyType>(r#""oco""#).is_err());
}

fn bracket_shaped_list() -> OrderList {
    let leg =
        |side, order_type, price, trigger_price, parent: Option<usize>, linked: Vec<usize>| {
            OrderListLeg {
                side,
                qty: 3.0,
                order_type,
                price,
                trigger_price,
                trail: None,
                reduce_only: parent.is_some(),
                time_in_force: TimeInForce::Gtc,
                parent,
                contingency: (!linked.is_empty()).then_some(ContingencyType::Oco),
                linked,
                order_client_id: None,
            }
        };
    OrderList {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        legs: vec![
            leg(1, OrderType::Limit, Some(100.0), None, None, vec![]),
            leg(-1, OrderType::Stop, None, Some(95.0), Some(0), vec![2]),
            leg(-1, OrderType::Limit, Some(110.0), None, Some(0), vec![1]),
        ],
        account: None,
    }
}

fn ids(n: usize) -> Vec<String> {
    ["E", "S", "T", "U", "V"][..n].iter().map(|s| s.to_string()).collect()
}

/// A list names every leg's order id or none: the ids it names are the ones it is built with, and a
/// list naming only some, an empty one or one twice is refused at the model's door. No id is the bytes
/// the leg always had.
#[test]
fn an_order_list_names_every_legs_order_id_or_none() {
    let mut spec = bracket_shaped_list();
    assert_eq!(spec.client_order_client_ids(), Ok(None));
    assert!(!serde_json::to_string(&spec.legs[0]).unwrap().contains("order_client_id"));
    for (leg, id) in spec.legs.iter_mut().zip(["E", "S", "T"]) {
        leg.order_client_id = Some(id.into());
    }
    assert_eq!(spec.client_order_client_ids(), Ok(Some(ids(3))));
    assert_eq!(build_order_list(&spec, &ids(3)).unwrap()[1].order_client_id, "S");
    assert!(build_order_list(&spec, &["E", "S", "X"].map(String::from)).is_err(), "not its ids");

    let mut some = spec.clone();
    some.legs[2].order_client_id = None;
    let err = build_order_list(&some, &ids(3)).unwrap_err();
    assert!(err.contains("every leg's order id or none: 2 of its 3"), "{err}");
    let mut twice = spec.clone();
    twice.legs[2].order_client_id = Some("S".into());
    assert!(twice.client_order_client_ids().unwrap_err().contains("named twice"));
    let mut empty = spec;
    empty.legs[0].order_client_id = Some(String::new());
    assert!(empty.client_order_client_ids().unwrap_err().contains("empty"));
}

/// ONE shape of linkage: the list a bracket is builds exactly the three requests `build_bracket` does.
#[test]
fn an_order_list_shaped_as_a_bracket_builds_the_bracket() {
    let spec = OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 3.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let built = build_order_list(&bracket_shaped_list(), &ids(3)).unwrap();
    assert_eq!(built, build_bracket(&spec, "E", "S", "T").to_vec());
}

/// The ladder: the entry is the OTO parent of the stop and every target; the stop (whole size) and
/// each target are linked OUO both ways; targets are not linked to one another.
#[test]
fn build_ladder_links_the_stop_and_each_target_ouo() {
    let spec = LadderSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 0.3,
        entry_price: None,
        stop_loss: 105.0,
        // sums to the entry's size in decimals, past it by a float residue: still a ladder
        targets: vec![
            LadderTarget { price: 95.0, qty: 0.1 },
            LadderTarget { price: 90.0, qty: 0.2 },
        ],
        account: None,
    };
    let [entry, stop, t1, t2] = <[OrderRequest; 4]>::try_from(
        build_order_list(&build_ladder(&spec).unwrap(), &ids(4)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        (entry.order_type.clone(), entry.side, entry.parent_order_id.clone()),
        (OrderType::Market, -1, None)
    );
    assert_eq!(entry.contingency_type, Some(ContingencyType::Oto));
    assert_eq!(entry.linked_order_ids, ids(4)[1..].to_vec(), "the entry links its three children");
    assert_eq!(
        (stop.order_type.clone(), stop.trigger_price, stop.qty),
        (OrderType::Stop, Some(105.0), 0.3)
    );
    assert_eq!(stop.linked_order_ids, vec!["T".to_string(), "U".to_string()]);
    for (t, px, qty) in [(&t1, 95.0, 0.1), (&t2, 90.0, 0.2)] {
        assert_eq!((t.order_type.clone(), t.price, t.qty), (OrderType::Limit, Some(px), qty));
        assert_eq!(t.linked_order_ids, vec!["S".to_string()], "a target links the stop alone");
    }
    for exit in [&stop, &t1, &t2] {
        assert_eq!(exit.contingency_type, Some(ContingencyType::Ouo));
        assert_eq!(exit.parent_order_id.as_deref(), Some("E"));
        assert!(exit.reduce_only && exit.side == 1, "{}", exit.order_client_id);
    }
    assert!([&entry, &stop, &t1, &t2].iter().all(|o| o.order_list_id.as_deref() == Some("E")));

    for (targets, why) in [
        (vec![], "one take-profit target"),
        (vec![LadderTarget { price: 95.0, qty: 0.0 }], "above zero"),
        (vec![LadderTarget { price: 95.0, qty: 0.2 }; 2], "past the entry"),
    ] {
        let err = build_ladder(&LadderSpec { targets, ..spec.clone() }).unwrap_err();
        assert!(err.contains(why), "{err}");
    }
}

/// A list whose links the contingency book cannot resolve is refused in words, nothing built.
#[test]
fn build_order_list_refuses_links_the_book_cannot_resolve() {
    type Edit = fn(&mut Vec<OrderListLeg>);
    let refused = |edit: Edit, n: usize| {
        let mut spec = bracket_shaped_list();
        edit(&mut spec.legs);
        build_order_list(&spec, &ids(n)).unwrap_err()
    };
    let cases: [(Edit, usize, &str); 8] = [
        (|l| l.truncate(1), 1, "two legs"),
        (|_| {}, 2, "order ids for 3 legs"),
        (|l| l[1].parent = Some(2), 3, "not an earlier leg"),
        (|l| l[1].contingency = Some(ContingencyType::Oto), 3, "OCO or OUO"),
        (|l| l[1].linked = vec![0], 3, "cannot link leg 0"),
        (|l| l[1].linked = vec![2, 2], 3, "cannot link leg 2"),
        (|l| l[2].contingency = Some(ContingencyType::Ouo), 3, "back with the same type"),
        (
            |l| {
                l[1].parent = None;
                l[1].linked.clear();
                l[1].contingency = None;
                l[2].linked.clear();
                l[2].contingency = None;
            },
            3,
            "leg 1 is linked to nothing",
        ),
    ];
    for (edit, n, why) in cases {
        let err = refused(edit, n);
        assert!(err.contains(why), "{why}: {err}");
    }
}

// ---- combo orders (PR-1: vocabulary + invariants) ----

fn call_spread() -> OrderCombo {
    OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            OrderComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1, limit: None },
        ],
        net_limit: Some(0.015),
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    }
}

#[test]
fn order_request_without_combo_legs_is_byte_identical() {
    // THE compatibility pin: an ordinary (non-combo) request must serialize EXACTLY as it did
    // before `combo_legs` existed — `skip_serializing_if = "Vec::is_empty"` means the key is
    // absent from the JSON entirely, so every existing fixture / journal record / wire body
    // round-trips unchanged. If this string ever needs editing, a persisted schema broke.
    let req = OrderRequest {
        order_client_id: "c".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(3.0),
        timestamp: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"order_client_id":"c","venue":"sim","symbol":"BTCUSDT","side":1,"qty":2.0,"order_type":"limit","price":3.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"timestamp":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}"#
    );
    assert!(!js.contains("combo_legs"));
    // and an OLD payload (no combo_legs key at all) still deserializes
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);
    assert!(back.combo_legs.is_empty());
}

#[test]
fn order_request_without_margin_mode_is_byte_identical() {
    // The margin-mode compatibility pin (twin of the combo_legs pin above): an ordinary
    // request with no requested mode serializes EXACTLY as before the field existed — the
    // `margin_mode` key is absent entirely, so every fixture / journal record / wire body
    // (and the journal `state_hash` fence) round-trips unchanged.
    let req = OrderRequest {
        order_client_id: "c".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(3.0),
        timestamp: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert!(!js.contains("margin_mode"), "{js}");
    // an OLD payload (no margin_mode key at all) still deserializes as None
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);
    assert_eq!(back.margin_mode, None);
    // and a requested mode round-trips
    let mut iso = req;
    iso.margin_mode = Some(crate::MarginMode::Isolated);
    let ijs = serde_json::to_string(&iso).unwrap();
    assert!(ijs.contains(r#""margin_mode":"Isolated""#), "{ijs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&ijs).unwrap(), iso);
}

#[test]
fn order_request_without_trigger_by_is_byte_identical() {
    // The trigger-source compatibility pin (twin of the margin_mode pin above): a request
    // with no requested source serializes EXACTLY as before the field existed — the
    // `trigger_by` key is absent entirely, so every fixture / journal record / wire body
    // (and the journal `state_hash` fence) round-trips unchanged.
    let req = OrderRequest {
        order_client_id: "c".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 2.0,
        order_type: OrderType::Stop,
        trigger_price: Some(95.0),
        timestamp: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert!(!js.contains("trigger_by"), "{js}");
    // an OLD payload (no trigger_by key at all) still deserializes as None
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);
    assert_eq!(back.trigger_by, None);
    // and a requested source round-trips
    let mut mark = req;
    mark.trigger_by = Some(TriggerBy::Mark);
    let mjs = serde_json::to_string(&mark).unwrap();
    assert!(mjs.contains(r#""trigger_by":"Mark""#), "{mjs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&mjs).unwrap(), mark);
}

#[test]
fn order_request_without_data_delay_is_byte_identical() {
    // The delay-label pin (decision 0140; twin of the trigger_by pin above): an unlabelled
    // request serializes to EXACTLY the bytes it had before the field existed — pinned against a
    // literal, not only "the key is absent" — so every journal record and the `state_hash` fence
    // are unchanged for an order no labelling feed priced.
    let req = OrderRequest {
        order_client_id: "c".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(190.0),
        timestamp: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"order_client_id":"c","venue":"ibkr","symbol":"AAPL.SMART.USD","side":1,"qty":1.0,"order_type":"limit","price":190.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"timestamp":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}"#
    );
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back.data_delay, None, "an old payload reads back unlabelled");
    // a labelled request carries the label and round-trips it
    let mut delayed = req;
    delayed.data_delay = Some(crate::StreamDelay::DELAYED);
    let djs = serde_json::to_string(&delayed).unwrap();
    assert!(djs.ends_with(r#","data_delay":{"Delayed":{"lag_ms":null}}}"#), "{djs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&djs).unwrap(), delayed);
}

/// The display and release pin (twin of the delay-label pin above): a request with neither
/// serializes to EXACTLY the bytes it had before the fields existed (pinned against a literal), an
/// old payload reads back with neither, and each set field rides LAST, in declaration order. A
/// `OrderRelease` names exactly its known conditions: a record carrying one this build does not know is
/// REFUSED, never read as a time-only release.
#[test]
fn order_request_without_display_or_release_is_byte_identical() {
    let req = OrderRequest {
        order_client_id: "c".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: OrderType::Limit,
        price: Some(190.0),
        timestamp: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"order_client_id":"c","venue":"ibkr","symbol":"AAPL.SMART.USD","side":1,"qty":10.0,"order_type":"limit","price":190.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"timestamp":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}"#
    );
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(
        (back.display_quantity, back.release),
        (None, None),
        "an old payload reads back as neither"
    );
    assert_eq!(back, req);

    let iceberg = OrderRequest {
        display_quantity: Some(2.0),
        release: Some(OrderRelease { at: 7, price: None }),
        ..req
    };
    let ijs = serde_json::to_string(&iceberg).unwrap();
    assert!(
        ijs.ends_with(r#""on_close":false,"display_quantity":2.0,"release":{"at":7}}"#),
        "{ijs}"
    );
    assert_eq!(serde_json::from_str::<OrderRequest>(&ijs).unwrap(), iceberg);

    let unknown = ijs.replace(r#"{"at":7}"#, r#"{"at":7,"price_above":100.0}"#);
    let err = serde_json::from_str::<OrderRequest>(&unknown).unwrap_err().to_string();
    assert!(err.contains("unknown field `price_above`"), "{err}");
    assert!(serde_json::from_str::<OrderRelease>("{}").is_err(), "a release states its instant");
}

/// The limit-offset pin: a request with no offset serializes to EXACTLY the bytes it had before the
/// field existed (a trailing request too: its `trail` was always written), an old payload reads back
/// with none, and a set one rides LAST.
#[test]
fn order_request_without_limit_offset_is_byte_identical() {
    let req = OrderRequest {
        order_client_id: "c".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Trailing,
        trigger_price: Some(105.0),
        trail: Some(2.0),
        timestamp: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"order_client_id":"c","venue":"okx","symbol":"BTC-USDT-SWAP","side":-1,"qty":1.0,"order_type":"trailing","price":null,"trigger_price":105.0,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"timestamp":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":2.0,"extreme":null,"on_close":false}"#
    );
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!((back.limit_offset, &back), (None, &req), "an old payload reads back with none");

    let limited =
        OrderRequest { order_type: OrderType::TrailingLimit, limit_offset: Some(0.5), ..req };
    let ljs = serde_json::to_string(&limited).unwrap();
    assert!(ljs.contains(r#""order_type":"trailing_limit""#), "{ljs}");
    assert!(ljs.ends_with(r#""on_close":false,"limit_offset":0.5}"#), "{ljs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&ljs).unwrap(), limited);
}

/// ⚠ An absent account must serialize to NOTHING. A single-account deployment's journal is
/// byte-identical before and after this field exists, which is what lets every stage of this
/// change stop safely. Asserted rather than reasoned about.
#[test]
fn an_account_less_order_serializes_exactly_as_it_did_before() {
    let req =
        OrderRequest { venue: "binance".into(), symbol: "BTCUSDT".into(), ..Default::default() };
    let json = serde_json::to_string(&req).expect("serialize");
    assert!(!json.contains("account"), "an absent account must not appear on the wire: {json}");
}

/// The other direction: a journal written before this field existed still deserializes.
#[test]
fn an_old_payload_with_no_account_still_deserializes() {
    let old = r#"{"order_client_id":"c1","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"market"}"#;
    let req: OrderRequest = serde_json::from_str(old).expect("an old payload must still parse");
    assert!(req.account.is_none());
}

#[test]
fn combo_request_serde_roundtrips() {
    let req = build_combo(&call_spread(), "K").unwrap();
    let js = serde_json::to_string(&req).unwrap();
    assert!(js.contains("combo_legs"), "{js}");
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);

    let spec = call_spread();
    let sjs = serde_json::to_string(&spec).unwrap();
    assert_eq!(serde_json::from_str::<OrderCombo>(&sjs).unwrap(), spec);
}

#[test]
fn build_combo_carries_signed_net_limit_and_legs() {
    let req = build_combo(&call_spread(), "K").unwrap();
    assert_eq!(req.order_client_id, "K");
    assert_eq!(req.venue, "deribit");
    assert_eq!(req.symbol, "", "combo instrument is resolved by the adapter at submit");
    assert_eq!(req.side, 1);
    assert_eq!(req.qty, 2.0);
    assert_eq!(req.order_type, OrderType::Limit);
    assert_eq!(req.price, Some(0.015));
    assert_eq!(req.combo_legs.len(), 2);
    assert_eq!(req.combo_legs[1].ratio, -1);

    // market combo: no net limit
    let mut spec = call_spread();
    spec.net_limit = None;
    let m = build_combo(&spec, "K2").unwrap();
    assert_eq!(m.order_type, OrderType::Market);
    assert_eq!(m.price, None);

    // CREDIT combo: the negative net limit passes through verbatim — no clamp, no abs()
    let mut credit = call_spread();
    credit.side = -1;
    credit.net_limit = Some(-0.0125);
    let c = build_combo(&credit, "K3").unwrap();
    assert_eq!(c.price, Some(-0.0125));
    assert_eq!(c.side, -1);
}

#[test]
fn combo_net_is_signed_sum_of_ratio_times_price() {
    // debit call spread: +1 @ 50, -1 @ 30  =>  net +20 (a DEBIT, positive)
    assert_eq!(combo_net(&[(1, 50.0), (-1, 30.0)]), 20.0);
    // credit put spread: -1 @ 50, +1 @ 30  =>  net -20 (a CREDIT, negative)
    assert_eq!(combo_net(&[(-1, 50.0), (1, 30.0)]), -20.0);
    // ratios scale
    assert_eq!(combo_net(&[(2, 10.0), (-1, 5.0)]), 15.0);
    // a balanced spread nets exactly zero — legal
    assert_eq!(combo_net(&[(1, 5.0), (-1, 5.0)]), 0.0);
    assert_eq!(combo_net(&[]), 0.0);
}

#[test]
fn combo_cross_buy_fills_at_or_below_limit() {
    let legs = [(1, 50.0), (-1, 30.0)]; // net = +20
    assert_eq!(combo_net_cross(1, 20.0, &legs), Some(20.0)); // exactly at the limit
    assert_eq!(combo_net_cross(1, 25.0, &legs), Some(20.0)); // better than the limit
    assert_eq!(combo_net_cross(1, 19.0, &legs), None); // too expensive
}

#[test]
fn combo_cross_sell_fills_at_or_above_limit() {
    let legs = [(1, 50.0), (-1, 30.0)]; // net = +20
    assert_eq!(combo_net_cross(-1, 20.0, &legs), Some(20.0)); // exactly at the limit
    assert_eq!(combo_net_cross(-1, 15.0, &legs), Some(20.0)); // better than the limit
    assert_eq!(combo_net_cross(-1, 25.0, &legs), None); // not enough credit
}

#[test]
fn combo_cross_handles_credit_negative_nets() {
    // A CREDIT structure: net is NEGATIVE on both sides of the law — nothing clamps to >= 0.
    let legs = [(-1, 50.0), (1, 30.0)]; // net = -20
    assert!(combo_net(&legs) < 0.0);
    // buying a credit combo at a -10 limit: -20 <= -10 => fills
    assert_eq!(combo_net_cross(1, -10.0, &legs), Some(-20.0));
    // ...but not at -30 (we'd need net <= -30)
    assert_eq!(combo_net_cross(1, -30.0, &legs), None);
    // selling it: net >= limit
    assert_eq!(combo_net_cross(-1, -30.0, &legs), Some(-20.0));
    assert_eq!(combo_net_cross(-1, -10.0, &legs), None);
    // a zero net crosses a zero limit from BOTH sides
    assert_eq!(combo_net_cross(1, 0.0, &[(1, 5.0), (-1, 5.0)]), Some(0.0));
    assert_eq!(combo_net_cross(-1, 0.0, &[(1, 5.0), (-1, 5.0)]), Some(0.0));
}

#[test]
fn combo_cross_never_fills_on_a_zero_side_or_nan() {
    // REGRESSION: `side >= 0` treated 0 (the i32 default, e.g. a garbage/partial spec) as a
    // BUY and could report a crossing. ±1 is the workspace-wide law; anything else = no fill.
    let legs = [(1, 50.0), (-1, 30.0)]; // net = +20, crosses a buy limit of 25
    assert_eq!(combo_net_cross(1, 25.0, &legs), Some(20.0), "sanity: +1 does cross");
    assert_eq!(combo_net_cross(0, 25.0, &legs), None, "side 0 is NOT a buy");
    assert_eq!(combo_net_cross(0, -25.0, &legs), None);
    // NaN anywhere => both comparisons false => no fill (the safe verdict)
    assert_eq!(combo_net_cross(1, f64::NAN, &legs), None);
    assert_eq!(combo_net_cross(-1, f64::NAN, &legs), None);
    assert_eq!(combo_net_cross(1, 25.0, &[(1, f64::NAN), (-1, 30.0)]), None);
}

#[test]
fn combo_net_fold_is_left_to_right_in_leg_order() {
    // The doc says the naive left-to-right fold IN LEG ORDER is part of the contract. Pin it
    // with values whose sum is association-sensitive: (a+b)+c != a+(b+c) in f64.
    let a = 1e16_f64;
    let legs = [(1, a), (1, 1.0), (-1, a)];
    // left-to-right: ((0 + 1e16) + 1) - 1e16 == 0.0 (the 1 is lost to rounding)
    assert_eq!(combo_net(&legs), 0.0);
    // a reordered fold would give 1.0 — proving the order is observable, not cosmetic
    let reordered = [(1, a), (-1, a), (1, 1.0)];
    assert_eq!(combo_net(&reordered), 1.0);
    assert_ne!(
        combo_net(&legs).to_bits(),
        combo_net(&reordered).to_bits(),
        "leg order must NOT be normalized away"
    );
}

#[test]
fn combo_net_from_legs_picks_ask_for_bought_legs_and_bid_for_sold() {
    // Bridges OrderCombo.legs -> combo_net, deciding the book side ONCE.
    let legs = call_spread().legs; // +1 100k call, -1 120k call
    let quote = |_symbol: &str, want_ask: bool| if want_ask { 0.030 } else { 0.010 };
    // BUYING the spread: pay the ASK on the +1 leg, receive the BID on the -1 leg.
    assert_eq!(combo_net_from_legs(1, &legs, quote), 0.030 - 0.010);
    // SELLING it flips both sides: receive the BID on the +1 leg, pay the ASK on the -1.
    assert_eq!(combo_net_from_legs(-1, &legs, quote), 0.010 - 0.030);
    // and it agrees with combo_net fed the same per-leg prices, in the same leg order
    assert_eq!(combo_net_from_legs(1, &legs, quote), combo_net(&[(1, 0.030), (-1, 0.010)]));
}

#[test]
fn combo_spec_rejects_degenerate_specs() {
    // REGRESSION (the leg-count invariant): a 0-leg spec used to lower to an ordinary-looking
    // NON-combo OrderRequest (empty `combo_legs` IS the "not a combo" sentinel) on an EMPTY
    // symbol at a NEGATIVE price. Both build_combo and Deserialize must refuse it.
    let mut zero = call_spread();
    zero.legs.clear();
    zero.net_limit = Some(-5.0);
    assert_eq!(zero.validate(), Err(ComboError::TooFewLegs(0)));
    assert_eq!(build_combo(&zero, "K"), Err(ComboError::TooFewLegs(0)));

    let mut one = call_spread();
    one.legs.truncate(1);
    assert_eq!(build_combo(&one, "K"), Err(ComboError::TooFewLegs(1)));

    let mut zero_ratio = call_spread();
    zero_ratio.legs[1].ratio = 0;
    assert_eq!(build_combo(&zero_ratio, "K"), Err(ComboError::ZeroRatio(1)));

    let mut empty_symbol = call_spread();
    empty_symbol.legs[0].symbol.clear();
    assert_eq!(build_combo(&empty_symbol, "K"), Err(ComboError::EmptyLegSymbol(0)));

    for bad in [0, 2, -3] {
        let mut s = call_spread();
        s.side = bad;
        assert_eq!(build_combo(&s, "K"), Err(ComboError::InvalidSide(bad)));
    }

    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut s = call_spread();
        s.qty = bad;
        assert_matches!(build_combo(&s, "K"), Err(ComboError::InvalidQuantity(_)));
    }

    let mut nan_limit = call_spread();
    nan_limit.net_limit = Some(f64::NAN);
    assert_matches!(build_combo(&nan_limit, "K"), Err(ComboError::NonFiniteNetLimit(_)));

    // ...while the legal shapes still build, including a CREDIT and a MARKET combo
    assert!(call_spread().validate().is_ok());
    let mut credit = call_spread();
    credit.side = -1;
    credit.net_limit = Some(-0.0125);
    assert_eq!(build_combo(&credit, "K").unwrap().price, Some(-0.0125));
    let mut mkt = call_spread();
    mkt.net_limit = None;
    assert!(build_combo(&mkt, "K").is_ok());
}

#[test]
fn combo_spec_deserialize_enforces_the_invariants() {
    // A REPLAYED journal record / wire body cannot smuggle an invalid spec past the gate.
    let js = r#"{"venue":"deribit","side":1,"qty":2.0,"legs":[],"net_limit":-5.0}"#;
    let err = serde_json::from_str::<OrderCombo>(js).unwrap_err().to_string();
    assert!(err.contains("2..=N legs"), "{err}");

    let one = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"BTC-PERPETUAL","ratio":1}],"net_limit":1.0}"#;
    assert!(serde_json::from_str::<OrderCombo>(one).is_err());

    let side0 = r#"{"venue":"deribit","side":0,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":1.0}"#;
    assert!(serde_json::from_str::<OrderCombo>(side0).is_err());

    // ...and a VALID one still decodes, with `time_in_force` ABSENT defaulting to Gtc
    // (the additive-field claim for OrderCombo itself).
    let ok = r#"{"venue":"deribit","side":-1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":-5.0}"#;
    let spec: OrderCombo = serde_json::from_str(ok).unwrap();
    assert_eq!(spec.time_in_force, TimeInForce::Gtc);
    assert_eq!(spec.net_limit, Some(-5.0), "credit net survives deserialize unclamped");
    assert_eq!(spec.legs.len(), 2);
}

#[test]
fn bracket_leg_shaped_request_is_byte_identical_too() {
    // Second compatibility pin: the byte-identity claim must hold for the CONTINGENCY-carrying
    // payloads that actually dominate the journal, not just a plain limit order.
    let req = OrderRequest {
        order_client_id: "S".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 3.0,
        order_type: OrderType::Stop,
        trigger_price: Some(95.0),
        reduce_only: true,
        timestamp: 7,
        parent_order_id: Some("E".into()),
        linked_order_ids: vec!["T".into()],
        order_list_id: Some("E".into()),
        contingency_type: Some(ContingencyType::Oco),
        on_close: true,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"order_client_id":"S","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":3.0,"order_type":"stop","price":null,"trigger_price":95.0,"reduce_only":true,"time_in_force":"GTC","gtd_expiry":null,"timestamp":7,"parent_order_id":"E","linked_order_ids":["T"],"order_list_id":"E","contingency_type":"OCO","weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":true}"#
    );
    assert!(!js.contains("combo_legs"));
    assert_eq!(serde_json::from_str::<OrderRequest>(&js).unwrap(), req);
}

#[test]
fn build_bracket_market_entry_short() {
    let spec = OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1, // short entry
        qty: 1.0,
        entry_price: None, // market entry
        stop_loss: 105.0,
        take_profit: 90.0,
        account: None,
        order_client_id: None,
    };
    let [entry, stop_loss, take_profit] = build_bracket(&spec, "E", "S", "T");
    assert_eq!(entry.order_type, OrderType::Market);
    assert_eq!(entry.price, None);
    // exits are the opposite (long) side, sized to the entry
    assert_eq!(stop_loss.side, 1);
    assert_eq!(take_profit.side, 1);
    assert_eq!(stop_loss.qty, 1.0);
    assert_eq!(take_profit.qty, 1.0);
}

/// `OrderType::KNOWN` is a hand list, kept whole the way `TimeInForce::ALL` is: `index` has one arm
/// per recognised variant and no wildcard, so a new variant stops this file compiling until it
/// gets an arm, and then `ARMS` and `KNOWN` must grow with it.
#[test]
fn order_type_known_is_every_recognised_variant_in_declaration_order() {
    const ARMS: usize = 10;
    fn index(k: &OrderType) -> Option<usize> {
        match k {
            OrderType::Market => Some(0),
            OrderType::Limit => Some(1),
            OrderType::Stop => Some(2),
            OrderType::StopLimit => Some(3),
            OrderType::TakeProfit => Some(4),
            OrderType::MarketToLimit => Some(5),
            OrderType::Trailing => Some(6),
            OrderType::TrailingLimit => Some(7),
            OrderType::Pegged => Some(8),
            OrderType::TrailingTakeProfit => Some(9),
            OrderType::Unrecognized(_) => None,
        }
    }
    assert_eq!(OrderType::KNOWN.len(), ARMS, "KNOWN and the arms of `idx` disagree");
    for (i, k) in OrderType::KNOWN.iter().enumerate() {
        assert_eq!(index(k), Some(i), "{k:?} is not at its own index of KNOWN");
        assert!(k.is_recognized(), "{k:?}");
    }
    assert!(!OrderType::Unrecognized("x".into()).is_recognized());
    assert_eq!(OrderType::default(), OrderType::Market);
}

/// Every recognised variant serializes as its lowercase word and reads back as itself; the
/// trigger set is exactly stop / stop_limit / take_profit / trailing / trailing_limit /
/// trailing_take_profit, and the trailing set its last three.
#[test]
fn order_type_round_trips_every_variant() {
    let words = [
        "market",
        "limit",
        "stop",
        "stop_limit",
        "take_profit",
        "market_to_limit",
        "trailing",
        "trailing_limit",
        "pegged",
        "trailing_take_profit",
    ];
    assert_eq!(OrderType::KNOWN.len(), words.len());
    for (k, word) in OrderType::KNOWN.iter().zip(words) {
        assert_eq!(k.as_str(), word);
        assert_eq!(k.to_string(), word);
        let json = serde_json::to_string(k).unwrap();
        assert_eq!(json, format!("\"{word}\""));
        assert_eq!(&serde_json::from_str::<OrderType>(&json).unwrap(), k);
        assert_eq!(&OrderType::from_stored(word), k);
    }
    let triggers: Vec<&OrderType> = OrderType::KNOWN.iter().filter(|k| k.is_trigger()).collect();
    assert_eq!(
        triggers,
        [
            &OrderType::Stop,
            &OrderType::StopLimit,
            &OrderType::TakeProfit,
            &OrderType::Trailing,
            &OrderType::TrailingLimit,
            &OrderType::TrailingTakeProfit
        ]
    );
    let trailing: Vec<&OrderType> = OrderType::KNOWN.iter().filter(|k| k.is_trailing()).collect();
    assert_eq!(
        trailing,
        [&OrderType::Trailing, &OrderType::TrailingLimit, &OrderType::TrailingTakeProfit]
    );
    assert!(!OrderType::Unrecognized("stop".into()).is_trigger());
    assert!(!OrderType::Unrecognized("trailing_stop".into()).is_trailing());
}

/// The STORED door: a word that is not EXACTLY ours (`"stop_market"`, `"LIMIT"`, `""`) reads as
/// `Unrecognized` with the text verbatim and writes back the very same bytes, both alone and
/// inside a whole `OrderRequest` — so a journal record or snapshot written before the type
/// existed re-serializes byte for byte. `from_stored` gives the same verdict as serde.
#[test]
fn order_type_stored_door_keeps_unknown_text_byte_identical() {
    for word in ["stop_market", "LIMIT", "Market", " limit", "", "trailing_stop", "TRAILING"] {
        let json = serde_json::to_string(word).unwrap();
        let read: OrderType = serde_json::from_str(&json).unwrap();
        assert_eq!(read, OrderType::Unrecognized(word.to_owned()), "{word:?}");
        assert_eq!(read, OrderType::from_stored(word), "{word:?}: from_stored agrees with serde");
        assert_eq!(serde_json::to_string(&read).unwrap(), json, "{word:?}: bytes unchanged");
    }
    for word in ["stop_market", "LIMIT"] {
        let doc = format!(
            r#"{{"order_client_id":"c","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"{word}","price":2.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"timestamp":0,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}}"#
        );
        let req: OrderRequest = serde_json::from_str(&doc).unwrap();
        assert_eq!(req.order_type.as_str(), word);
        assert_eq!(serde_json::to_string(&req).unwrap(), doc, "{word}: request bytes unchanged");
    }
    // a non-string is refused exactly as the `String` field refused it
    assert!(serde_json::from_str::<OrderType>("3").is_err());
    assert!(serde_json::from_str::<OrderType>("null").is_err());
}

/// The INPUT door: trimmed and ASCII case-insensitive over the five words, nothing else — no
/// synonyms, never `Unrecognized` — and the refusal names every accepted word.
#[test]
fn order_type_parse_is_case_insensitive_trims_and_refuses_the_rest() {
    assert_eq!(OrderType::parse("limit"), Ok(OrderType::Limit));
    assert_eq!(OrderType::parse("LIMIT"), Ok(OrderType::Limit));
    assert_eq!(OrderType::parse(" Limit "), Ok(OrderType::Limit));
    assert_eq!(OrderType::parse("Stop_Limit"), Ok(OrderType::StopLimit));
    assert_eq!(OrderType::parse("TAKE_PROFIT\n"), Ok(OrderType::TakeProfit));
    assert_eq!(OrderType::parse("Market_To_Limit"), Ok(OrderType::MarketToLimit));
    assert_eq!(OrderType::parse(" Trailing"), Ok(OrderType::Trailing));
    assert_eq!(OrderType::parse("TRAILING_LIMIT"), Ok(OrderType::TrailingLimit));
    for bad in ["", "  ", "garbage", "stop_market", "mkt", "trailing_stop", "stop-limit", "mtl"] {
        let err = OrderType::parse(bad).unwrap_err();
        assert_eq!(err.input, bad);
        assert_eq!(
            err.to_string(),
            format!(
                "unknown order type {bad:?}: expected one of market, limit, stop, stop_limit, \
                 take_profit, market_to_limit, trailing, trailing_limit, pegged, \
                 trailing_take_profit"
            )
        );
    }
    assert_eq!(OrderType::parse(" Pegged"), Ok(OrderType::Pegged));
    assert_eq!(OrderType::parse("Trailing_Take_Profit"), Ok(OrderType::TrailingTakeProfit));
}

/// A refused lifetime word gets the order type's sentence shape (`write_unknown_word`, the two
/// errors' one writer): the input verbatim, then every accepted word in `TimeInForce::ALL` order.
#[test]
fn an_unknown_time_in_force_names_its_input_and_every_accepted_word() {
    let err = TimeInForce::parse("gtx").unwrap_err();
    assert_eq!(
        err.to_string(),
        "unknown time in force \"gtx\": expected one of gtc, ioc, fok, gtd, day, post-only, \
         at-open, at-close"
    );
}

/// The side vocabulary every order-placing text surface reads (the CLI, Telegram): six words, any
/// ASCII case, and nothing else — a word outside it is `None`, never a guessed side.
#[test]
fn side_from_word_reads_six_words_in_any_case_and_nothing_else() {
    for w in ["buy", "b", "long", "BUY", "Long", "B"] {
        assert_eq!(side_from_word(w), Some(1), "{w}");
    }
    for w in ["sell", "s", "short", "SELL", "Short", "S"] {
        assert_eq!(side_from_word(w), Some(-1), "{w}");
    }
    for w in ["", "bid", "ask", "buy ", "1", "-1", "longs", "sel"] {
        assert_eq!(side_from_word(w), None, "{w:?}");
    }
}

// ---- wave 6 of the order kinds: a combo priced by its legs, its account, an option exercise ----

/// `call_spread` priced by its legs: buy the 100k call at 0.03 or less, sell the 120k at 0.01 or more.
fn leg_limited_spread() -> OrderCombo {
    let mut spec = call_spread();
    spec.net_limit = None;
    spec.legs[0].limit = Some(0.03);
    spec.legs[1].limit = Some(0.01);
    spec
}

/// A combo that states no leg limit and no account writes no key for either: every spec, request and
/// journal record written before the two fields keeps its bytes, and the old bytes read back as `None`.
#[test]
fn a_combo_without_leg_limits_or_an_account_keeps_its_bytes() {
    let legs = serde_json::to_string(&call_spread().legs).unwrap();
    assert_eq!(
        legs,
        r#"[{"symbol":"BTC-27MAR26-100000-C","ratio":1},{"symbol":"BTC-27MAR26-120000-C","ratio":-1}]"#
    );
    let spec = serde_json::to_string(&call_spread()).unwrap();
    assert!(!spec.contains("account") && !spec.contains("order_client_id"), "{spec}");
    let req = serde_json::to_string(&build_combo(&call_spread(), "K").unwrap()).unwrap();
    assert!(!req.contains(r#""limit":"#) && !req.contains("account"), "{req}");
    let old = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":1.0}"#;
    let spec: OrderCombo = serde_json::from_str(old).unwrap();
    assert!(spec.account.is_none() && spec.legs.iter().all(|l| l.limit.is_none()));
    assert_eq!(spec.order_client_id, None, "an old spec names no id: the runtime mints one");
}

/// A combo its sender named an id for carries it under its own key, and reads it back.
#[test]
fn a_combo_carries_the_id_its_sender_minted() {
    let named = OrderCombo { order_client_id: Some("c1combo".into()), ..call_spread() };
    let bytes = serde_json::to_string(&named).unwrap();
    assert!(bytes.ends_with(r#","order_client_id":"c1combo"}"#), "{bytes}");
    assert_eq!(serde_json::from_str::<OrderCombo>(&bytes).unwrap(), named);
}

/// A combo priced by its legs lowers to a `Limit` with NO price (its limits ride on the legs), so a
/// reader that knows only the net refuses it rather than sending it at market; it round-trips.
#[test]
fn a_combo_priced_by_its_legs_is_a_limit_with_no_net() {
    let spec = leg_limited_spread();
    assert_eq!(spec.validate(), Ok(()));
    let req = build_combo(&spec, "K").unwrap();
    assert_eq!(req.order_type, OrderType::Limit);
    assert_eq!(req.price, None);
    assert_eq!(req.combo_legs[0].limit, Some(0.03));
    assert_eq!(req.combo_legs[1].limit, Some(0.01));
    let js = serde_json::to_string(&spec).unwrap();
    assert!(js.contains(r#""ratio":1,"limit":0.03}"#), "{js}");
    assert_eq!(serde_json::from_str::<OrderCombo>(&js).unwrap(), spec);
    let back: OrderRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(back, req);
}

/// The leg-limit terms, refused in words by `validate`, `build_combo` and `Deserialize` alike.
#[test]
fn a_leg_limit_combo_refuses_a_net_a_missing_leg_or_a_non_finite_limit() {
    let mut both = leg_limited_spread();
    both.net_limit = Some(0.02);
    assert_eq!(build_combo(&both, "K"), Err(ComboError::NetAndLegLimits));
    let mut one = leg_limited_spread();
    one.legs[1].limit = None;
    assert_eq!(build_combo(&one, "K"), Err(ComboError::MissingLegLimit(1)));
    let mut nan = leg_limited_spread();
    nan.legs[0].limit = Some(f64::NAN);
    assert_matches!(build_combo(&nan, "K"), Err(ComboError::NonFiniteLegLimit(0, _)));
    let half = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1,"limit":1.0},{"symbol":"B","ratio":-1}]}"#;
    let err = serde_json::from_str::<OrderCombo>(half).unwrap_err().to_string();
    assert!(err.contains("every leg states one or none does"), "{err}");
}

/// One instrument in two legs is refused in words by `validate`, `build_combo` and `Deserialize`
/// alike: the simulator and the paper book read the same rule.
#[test]
fn a_combo_refuses_a_repeated_leg_instrument() {
    let mut twice = call_spread();
    twice.legs[1].symbol = twice.legs[0].symbol.clone();
    let err = build_combo(&twice, "K").unwrap_err();
    assert_eq!(err, ComboError::RepeatedLegSymbol("BTC-27MAR26-100000-C".into()));
    assert!(err.to_string().contains("BTC-27MAR26-100000-C twice"), "{err}");
    let twice = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"A","ratio":-1}]}"#;
    let err = serde_json::from_str::<OrderCombo>(twice).unwrap_err().to_string();
    assert!(err.contains("A twice"), "{err}");
}

/// Any lifetime is the order's (a venue may take an IOC, FOK or GTD combo, deribit does), so
/// `validate`, `build_combo` and `Deserialize` take it; only the two books' rule refuses all but GTC.
#[test]
fn a_combo_takes_every_lifetime_and_only_the_books_rule_refuses_all_but_gtc() {
    assert_eq!(call_spread().book_lifetime_refusal(), None, "a GTC combo rests on either book");
    for tif in [TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day, TimeInForce::Gtd] {
        let mut s = call_spread();
        s.time_in_force = tif;
        assert_eq!(build_combo(&s, "K").unwrap().time_in_force, tif, "the order keeps {tif:?}");
        let err = s.book_lifetime_refusal().expect("refused by the books");
        assert_eq!(err, ComboError::NotGtc(tif));
        assert!(err.to_string().starts_with("a combo rests as a GTC order, not a"), "{err}");
    }
    let ioc = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"time_in_force":"IOC"}"#;
    let spec: OrderCombo = serde_json::from_str(ioc).unwrap();
    assert_eq!(spec.time_in_force, TimeInForce::Ioc);
}

/// The shared per-leg law: every leg crosses its own limit at its own side of the touch, or nothing
/// fills; a bought leg reads the ask, a sold leg the bid, and selling the combo flips both.
#[test]
fn the_leg_cross_law_is_all_or_nothing_at_each_legs_own_side() {
    let legs = leg_limited_spread().legs; // +1 100k call at <= 0.03, -1 120k call at >= 0.01
    let book = |ask_a: f64, bid_b: f64| {
        move |symbol: &str, want_ask: bool| match (symbol, want_ask) {
            ("BTC-27MAR26-100000-C", true) => ask_a,
            ("BTC-27MAR26-120000-C", false) => bid_b,
            _ => f64::NAN,
        }
    };
    assert_eq!(combo_legs_cross(1, &legs, book(0.03, 0.01)), Some(vec![0.03, 0.01]), "at both");
    assert_eq!(combo_legs_cross(1, &legs, book(0.02, 0.015)), Some(vec![0.02, 0.015]), "better");
    assert_eq!(combo_legs_cross(1, &legs, book(0.031, 0.02)), None, "one leg short: nothing");
    assert_eq!(combo_legs_cross(1, &legs, book(0.02, 0.009)), None, "the other leg short");
    // selling the combo sells the first leg (its bid >= 0.03) and buys the second (its ask <= 0.01)
    let flipped = |symbol: &str, want_ask: bool| match (symbol, want_ask) {
        ("BTC-27MAR26-100000-C", false) => 0.04,
        ("BTC-27MAR26-120000-C", true) => 0.005,
        _ => f64::NAN,
    };
    assert_eq!(combo_legs_cross(-1, &legs, flipped), Some(vec![0.04, 0.005]));
    assert_eq!(combo_legs_cross(0, &legs, book(0.02, 0.02)), None, "side 0 is not a buy");
    assert_eq!(combo_legs_cross(1, &legs, book(f64::NAN, 0.02)), None, "no price, no fill");
    assert_eq!(combo_legs_cross(1, &legs, book(0.02, f64::INFINITY)), None, "an infinite bid");
    assert_eq!(combo_legs_cross(1, &call_spread().legs, book(0.0, 1.0)), None, "no leg limits");
    assert_eq!(combo_legs_cross(1, &[], book(0.0, 1.0)), None, "no legs");
}

/// A combo names its account on the one request it is, `Some(Default)` included (a NAME, which
/// routes differently from `None` on a venue that runs two engines), and round-trips with it.
#[test]
fn build_combo_carries_the_account() {
    use crate::accounts::account_keys::AccountLabel;
    for account in [AccountLabel::parse("PERP").unwrap(), AccountLabel::Default] {
        let mut spec = call_spread();
        spec.account = Some(account.clone());
        assert_eq!(build_combo(&spec, "K").unwrap().account, Some(account.clone()));
        let js = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<OrderCombo>(&js).unwrap(), spec, "{js}");
    }
}

/// An exercise names its option and a quantity above zero, keeps its bytes without an account, and
/// refuses a key it does not know (a later term is never dropped).
#[test]
fn an_exercise_names_its_option_and_a_quantity_above_zero() {
    let spec = ExerciseSpec {
        venue: "ibkr".into(),
        symbol: "AAPL 20261218C200".into(),
        qty: 2.0,
        account: None,
    };
    assert_eq!(spec.validate(), Ok(()));
    let js = serde_json::to_string(&spec).unwrap();
    assert_eq!(js, r#"{"venue":"ibkr","symbol":"AAPL 20261218C200","qty":2.0}"#);
    assert_eq!(serde_json::from_str::<ExerciseSpec>(&js).unwrap(), spec);
    let later = r#"{"venue":"ibkr","symbol":"AAPL 20261218C200","qty":2.0,"lapse":true}"#;
    assert!(serde_json::from_str::<ExerciseSpec>(later).is_err(), "an unknown term is refused");
    let unnamed = ExerciseSpec { symbol: String::new(), ..spec.clone() };
    assert!(unnamed.validate().unwrap_err().contains("names none"));
    for qty in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let bad = ExerciseSpec { qty, ..spec.clone() };
        assert!(bad.validate().unwrap_err().contains("not a finite number above zero"), "{qty}");
    }
}
