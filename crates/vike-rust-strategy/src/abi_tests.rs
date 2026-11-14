use super::*;

/// A `BarAbi` must be plain data: no padding surprises, no Rust types.
/// Pinned because a field added without `#[repr(C)]` discipline silently
/// changes the layout both sides agreed on.
#[test]
fn cbar_layout_is_pinned() {
    assert_eq!(std::mem::size_of::<OptionAbi<f64>>(), 16);
    assert_eq!(std::mem::align_of::<BarAbi>(), 8);
    // ts + 5 f64 + 3 OptionAbi<f64> + symbol ptr/len
    assert_eq!(std::mem::size_of::<BarAbi>(), 8 + 5 * 8 + 3 * 16 + 16);
}

// `abi_version_is_nonzero` used to live here as a `#[test]` asserting `ABI_VERSION > 0`.
// `clippy::assertions_on_constants` (a `-D warnings` merge-gate lint) correctly flags an
// `assert!` whose condition is knowable at compile time — the same claim now lives as a
// `const _: () = assert!(..)` beside `ABI_VERSION`'s own declaration, which is CHECKED ON
// EVERY BUILD rather than only under `cargo test`, so nothing was lost by deleting the test.

/// `from_bar` -> `to_bar` must be lossless: every optional survives, including a `None` one
/// (which must NOT come back as `Some(NaN)` — `OptionAbi<f64>`'s `present` flag, not the NaN sentinel
/// in its `value`, is what `Into<Option<f64>>` reads).
#[test]
fn cbar_round_trip_preserves_optionals_and_symbol() {
    let original = vike_marketdata::Bar {
        timestamp: 1_700_000_000_000,
        open: 100.0,
        high: 101.5,
        low: 99.5,
        close: 100.25,
        volume: 42.0,
        funding: Some(0.01),
        bid: None,
        ask: Some(1.6),
        symbol: Some("BTCUSDT".to_string()),
    };

    let c = BarAbi::from_bar(&original);
    let round_tripped = c.to_bar();

    assert_eq!(round_tripped, original);
    assert_eq!(round_tripped.funding, Some(0.01));
    assert_eq!(round_tripped.bid, None, "an absent optional must not come back as Some(NaN)");
    assert_eq!(round_tripped.ask, Some(1.6));
    assert_eq!(round_tripped.symbol.as_deref(), Some("BTCUSDT"));
}

/// Track A review finding #1: the null-`symbol_ptr` branch (a `Bar` whose `symbol` is `None`)
/// was covered by no test — only the `Some("BTCUSDT")` path ran. `from_bar` must produce a
/// null `symbol_ptr`/zero `symbol_len` for a `None` symbol, and `to_bar` must read that back
/// as `None` rather than dereferencing a null pointer or fabricating an empty string.
#[test]
fn cbar_round_trip_preserves_a_missing_symbol() {
    let original = vike_marketdata::Bar {
        timestamp: 1,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };

    let c = BarAbi::from_bar(&original);
    assert!(c.symbol_ptr.is_null(), "a None symbol must cross as a null pointer");
    assert_eq!(c.symbol_len, 0);

    let round_tripped = c.to_bar();
    assert_eq!(round_tripped, original);
    assert_eq!(round_tripped.symbol, None);
}

// ---- ABI_VERSION 3: the tier-2 mirrors ----

/// Every field of every tier-2 mirror must survive the round trip. One test over all five
/// rather than five near-identical ones: the failure they guard against is a field FORGOTTEN
/// in `of` or in `to_*`, and a forgotten field is equally invisible in whichever mirror it
/// sits in. Every value below is distinct and non-default, so a field left at `Default` or
/// copied from its neighbour fails rather than passing on a coincidence.
#[test]
fn the_tier_two_mirrors_round_trip_every_field() {
    let q = vike_marketdata::TickQuote {
        timestamp: 1_700_000_000_001,
        local_timestamp: 1_700_000_000_002,
        bid: 99.5,
        ask: 100.5,
        bid_size: 3.25,
        ask_size: 4.75,
        symbol: "BTCUSDT".to_string(),
    };
    assert_eq!(TickQuoteAbi::of(&q).to_quote_tick(), q);

    let t = vike_marketdata::TickTrade {
        timestamp: 1_700_000_000_003,
        local_timestamp: 1_700_000_000_004,
        price: 100.25,
        size: 0.5,
        is_buyer_maker: true,
        symbol: "ETHUSDT".to_string(),
    };
    assert_eq!(TickTradeAbi::of(&t).to_trade_tick(), t);

    let m = vike_model::TickMark {
        symbol: "btcusdt".to_string(),
        price: 64_321.5,
        timestamp: 1_700_000_000_005,
    };
    assert_eq!(TickMarkAbi::of(&m).to_mark_tick(), m);

    let f = vike_model::Fill {
        side: -1,
        size: 2.5,
        price: 101.75,
        fee: 0.0625,
        timestamp: 1_700_000_000_006,
        is_maker: true,
        symbol: "SOLUSDT".to_string(),
        tag: Some("long#3".to_string()),
    };
    assert_eq!(FillAbi::of(&f).to_fill(), f);
    let untagged = vike_model::Fill { tag: None, ..f.clone() };
    assert_eq!(FillAbi::of(&untagged).to_fill(), untagged, "an untagged fill stays untagged");

    let fl = vike_model::FlowToxicity { bid: 0.25, ask: 0.75, timestamp: 1_700_000_000_007 };
    assert_eq!(FlowToxicityAbi::of(fl).to_flow(), fl);
}

/// An EMPTY symbol must come back empty, not as a panic and not as a fabricated value. The
/// tick types carry `symbol: String` (not `Option<String>` like `Bar`), and "" is the
/// documented single-symbol-path value — so this is the ordinary case on those paths, not an
/// edge one. It exercises [`read_borrowed_str`]'s zero-length early return, which is the
/// branch that keeps a null pointer from ever reaching `from_raw_parts`.
#[test]
fn an_empty_symbol_crosses_as_empty_rather_than_panicking() {
    let q = vike_marketdata::TickQuote {
        timestamp: 1,
        local_timestamp: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    assert_eq!(TickQuoteAbi::of(&q).to_quote_tick().symbol, "");
}

/// Both former copies' panic text, byte for byte (`expect`'s `"{msg}: {err:?}"` shape): this side
/// says "borrowed string", the host's `read_str` says "symbol". A valid pair reads back unchanged.
#[test]
fn read_borrowed_str_reads_valid_bytes_and_names_what_it_refused() {
    let s = "BTCUSDT";
    assert_eq!(read_borrowed_str(s.as_ptr(), s.len(), "symbol"), "BTCUSDT");
    let bad = [0xffu8, 0xfe];
    let utf8_err = String::from_utf8(bad.to_vec()).unwrap_err();
    for what in ["borrowed string", "symbol"] {
        let payload = std::panic::catch_unwind(|| read_borrowed_str(bad.as_ptr(), bad.len(), what))
            .expect_err("invalid UTF-8 must panic");
        assert_eq!(
            payload.downcast_ref::<String>().expect("a formatted panic"),
            &format!(
                "strategy file ABI contract violated: {what} bytes are not valid UTF-8: {utf8_err:?}"
            )
        );
    }
}

// ---- ABI_VERSION 3: the integer mappings ----

/// Every `FeedStatus` variant must survive its code, and an unknown code must be REFUSED
/// rather than defaulted — a fabricated `Disconnected` would make a strategy pull quotes that
/// were never in danger.
#[test]
fn every_feed_status_round_trips_and_an_unknown_code_is_refused() {
    for s in [
        vike_model::FeedStatus::Disconnected,
        vike_model::FeedStatus::Stale,
        vike_model::FeedStatus::Live,
    ] {
        assert_eq!(feed_status_from_code(feed_status_code(s)), Some(s));
    }
    assert_eq!(feed_status_from_code(3), None);
    assert_eq!(feed_status_from_code(u32::MAX), None);
}

/// Every `OrderEventKind` variant must survive `(code, reason)`, INCLUDING the reason text of
/// the three that carry one — and an unknown code must be refused. A `Canceled` invented from
/// a code this host does not know would make a live order look dead and free a slot that is
/// still occupied.
#[test]
fn every_order_event_kind_round_trips_with_its_reason() {
    let kinds = [
        vike_model::OrderEventKind::Accepted,
        vike_model::OrderEventKind::Rejected { reason: "min notional".to_string() },
        vike_model::OrderEventKind::Denied { reason: "risk gate veto".to_string() },
        vike_model::OrderEventKind::Canceled { reason: "operator pull".to_string() },
        vike_model::OrderEventKind::Expired,
        vike_model::OrderEventKind::Filled,
    ];
    for k in &kinds {
        let code = order_event_code(k);
        let reason = order_event_reason(k).to_string();
        assert_eq!(order_event_from_code(code, reason).as_ref(), Some(k));
    }
    assert_eq!(order_event_from_code(6, String::new()), None);
}

/// ...and the whole `OrderEvent`, through the mirror, with the `tag` present and absent.
/// The absent case is the one that would silently pass on a null-pointer heuristic: `Some("")`
/// and `None` are different facts about an order, and only `OptionAbi::<StrAbi>::present` separates
/// them.
#[test]
fn an_order_lifecycle_round_trips_including_an_absent_tag() {
    let tagged = vike_model::OrderEvent {
        order_client_id: "vike-1".to_string(),
        tag: Some("bid-1".to_string()),
        kind: vike_model::OrderEventKind::Canceled { reason: "replaced".to_string() },
    };
    assert_eq!(OrderEventAbi::of(&tagged).to_order_lifecycle(), Some(tagged.clone()));

    let untagged = vike_model::OrderEvent {
        order_client_id: "vike-2".to_string(),
        tag: None,
        kind: vike_model::OrderEventKind::Accepted,
    };
    let back = OrderEventAbi::of(&untagged).to_order_lifecycle();
    assert_eq!(back, Some(untagged));

    let empty_tag = vike_model::OrderEvent {
        order_client_id: "vike-3".to_string(),
        tag: Some(String::new()),
        kind: vike_model::OrderEventKind::Filled,
    };
    assert_eq!(
        OrderEventAbi::of(&empty_tag).to_order_lifecycle(),
        Some(empty_tag),
        "an EMPTY tag is not an ABSENT tag — see OptionAbi<StrAbi>'s own doc"
    );
}

/// A `OrderEventAbi` carrying a kind code this host does not know is refused whole, rather
/// than delivered with a guessed kind.
#[test]
fn an_unknown_lifecycle_kind_refuses_the_whole_event() {
    let order_client_id = "vike-9";
    let event = OrderEventAbi {
        order_client_id: StrAbi::of(order_client_id),
        tag: OptionAbi::<StrAbi>::of(None),
        kind: 99,
        reason: StrAbi::empty(),
    };
    assert!(event.to_order_lifecycle().is_none());
}

// ---- ABI_VERSION 4: the contract value ----

/// Both kinds and `None` survive the mirror, and an unknown kind code is `None`, never a guessed
/// linear (an inverse contract read as linear is wrong by the price).
#[test]
fn a_contract_value_round_trips_and_an_unknown_kind_is_refused() {
    use vike_model::ContractValue;
    for v in [None, Some(ContractValue::linear(50.0)), Some(ContractValue::inverse(100.0))] {
        assert_eq!(Option::<ContractValue>::from(ContractValueOptionAbi::from(v)), v);
    }
    let unknown = ContractValueOptionAbi { kind: 2, per_unit: 1.0, present: true };
    assert_eq!(Option::<ContractValue>::from(unknown), None);
}

// ---- ABI_VERSION 5: the order seam's mirrors ----

/// `None`, an empty grid and a full one survive the mirror field by field.
#[test]
fn an_instrument_grid_round_trips_including_none() {
    use vike_model::Grid;
    let full = Grid {
        tick_size: Some(0.05),
        lot_size: Some(0.001),
        min_notional: Some(5.0),
        min_qty: Some(0.002),
    };
    let empty = Grid { tick_size: None, lot_size: None, min_notional: None, min_qty: None };
    for v in [None, Some(empty), Some(full), Some(Grid { lot_size: None, ..full })] {
        assert_eq!(Option::<Grid>::from(GridOptionAbi::from(v)), v);
    }
}

/// Every `OrderPlacement` kind survives the mirror with its prices and its optionals; an unknown code
/// and a stop-limit with no limit are refused, never read as some other order.
#[test]
fn every_tagged_order_round_trips_and_an_undecodable_one_is_refused() {
    use vike_model::OrderPlacement as T;
    for o in [
        T::Market,
        T::Limit { price: 100.25 },
        T::Stop { trigger: 99.5 },
        T::StopLimit { trigger: 99.5, limit: 99.25 },
        T::TakeProfit { trigger: 101.0, limit: None },
        T::TakeProfit { trigger: 101.0, limit: Some(101.5) },
        T::Trailing { trail: 0.5, activation: None },
        T::Trailing { trail: 0.75, activation: Some(102.0) },
        T::Iceberg { price: 100.25, display: 0.5 },
    ]
    .into_iter()
    .chain(
        vike_model::OrderPegAnchor::ALL
            .map(|anchor| T::Pegged { peg: vike_model::OrderPeg { anchor, offset: -0.25 } }),
    ) {
        assert_eq!(OrderPlacementAbi::of(o).to_tagged_order(), Some(o));
    }
    let unknown = OrderPlacementAbi { kind: 99, ..OrderPlacementAbi::of(T::Market) };
    assert_eq!(unknown.to_tagged_order(), None);
    let peg = vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Mid, offset: 0.0 };
    let no_anchor = OrderPlacementAbi { anchor: 4, ..OrderPlacementAbi::of(T::Pegged { peg }) };
    assert_eq!(no_anchor.to_tagged_order(), None, "an anchor past OrderPegAnchor::ALL is refused");
    let stop_limit = OrderPlacementAbi::of(T::StopLimit { trigger: 99.5, limit: 99.25 });
    let no_limit = OrderPlacementAbi { limit: None.into(), ..stop_limit };
    assert_eq!(no_limit.to_tagged_order(), None);
}

/// A NaN or an infinity in an order list crosses as itself, so the host refuses it in the words a
/// native order meets (JSON alone would hand it back as `None`, refused for a missing price); a
/// finite list is the plain serde JSON; text that is not a number is an error, never a guess.
#[test]
fn an_order_lists_non_finite_numbers_cross_as_themselves() {
    use vike_model::{OrderListLeg, OrderType, TimeInForce};
    let leg = OrderListLeg {
        side: -1,
        qty: f64::NAN,
        order_type: OrderType::Limit,
        price: Some(f64::NAN),
        trigger_price: Some(f64::INFINITY),
        trail: Some(f64::NEG_INFINITY),
        reduce_only: true,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: vec![],
        order_client_id: None,
    };
    let legs = vec![("x#1".to_string(), leg.clone())];
    let back = order_list_of_json(&order_list_json(&legs)).expect("its own text decodes");
    assert_eq!(format!("{back:?}"), format!("{legs:?}"));
    let words = |l: &OrderListLeg| {
        vike_model::order_terms_refusal(
            &l.order_type,
            l.price,
            l.trigger_price,
            l.trail,
            None,
            None,
            None,
        )
    };
    assert_eq!(words(&back[0].1).as_deref(), Some("the price NaN is not a finite number"));

    let finite =
        OrderListLeg { qty: 2.0, price: Some(101.5), trigger_price: None, trail: None, ..leg };
    let finite = vec![("y#1".to_string(), finite)];
    let plain: Vec<(String, OrderListLeg)> =
        serde_json::from_str(&order_list_json(&finite)).expect("a finite list is plain serde JSON");
    assert_eq!(plain, finite);
    assert!(
        order_list_of_json(r#"[["z#1", {"side": 1, "qty": "lots", "order_type": "market"}]]"#)
            .is_err()
    );
}
