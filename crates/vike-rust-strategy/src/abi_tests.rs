use super::*;

/// The two types a strategy file reads in place (`ABI_VERSION` 14) are what [`layout`] hashes: `Bar` 120
/// bytes and `TradeClosedRow` 136, both 8-aligned, every field at an ascending offset in declaration order
/// (`repr(C)`), so a reorder or a new field moves the hash and refuses a stale file at load. The hash is
/// not 0, the answer a file's export gives after a caught panic.
#[test]
fn the_in_place_layouts_are_what_layout_hashes() {
    use std::mem::{align_of, offset_of, size_of};
    use vike_marketdata::Bar;
    use vike_model::TradeClosedRow;
    // 104: the symbol is an `Option<vike_marketdata::Symbol>`, one pointer (120 with an `Option<String>`).
    assert_eq!((size_of::<Bar>(), align_of::<Bar>()), (104, 8));
    assert_eq!((size_of::<TradeClosedRow>(), align_of::<TradeClosedRow>()), (136, 8));
    let bar = [
        offset_of!(Bar, timestamp),
        offset_of!(Bar, open),
        offset_of!(Bar, high),
        offset_of!(Bar, low),
        offset_of!(Bar, close),
        offset_of!(Bar, volume),
        offset_of!(Bar, funding),
        offset_of!(Bar, bid),
        offset_of!(Bar, ask),
        offset_of!(Bar, symbol),
    ];
    let row = [
        offset_of!(TradeClosedRow, symbol),
        offset_of!(TradeClosedRow, tag),
        offset_of!(TradeClosedRow, side),
        offset_of!(TradeClosedRow, entry_timestamp),
        offset_of!(TradeClosedRow, entry_bar),
        offset_of!(TradeClosedRow, entry_price),
        offset_of!(TradeClosedRow, exit_timestamp),
        offset_of!(TradeClosedRow, exit_bar),
        offset_of!(TradeClosedRow, exit_price),
        offset_of!(TradeClosedRow, qty),
        offset_of!(TradeClosedRow, pnl),
        offset_of!(TradeClosedRow, commission),
        offset_of!(TradeClosedRow, net),
    ];
    assert!(bar.is_sorted() && bar[0] == 0, "Bar's fields in declaration order: {bar:?}");
    assert!(
        row.is_sorted() && row[0] == 0,
        "TradeClosedRow's fields in declaration order: {row:?}"
    );
    // `ABI_VERSION` 15: the tick hooks lend the host's own tick, so the two tick types are `repr(C)` too.
    use vike_marketdata::{TickQuote, TickTrade};
    let quote = [
        offset_of!(TickQuote, timestamp),
        offset_of!(TickQuote, local_timestamp),
        offset_of!(TickQuote, bid),
        offset_of!(TickQuote, ask),
        offset_of!(TickQuote, bid_size),
        offset_of!(TickQuote, ask_size),
        offset_of!(TickQuote, symbol),
    ];
    let trade = [
        offset_of!(TickTrade, timestamp),
        offset_of!(TickTrade, local_timestamp),
        offset_of!(TickTrade, price),
        offset_of!(TickTrade, size),
        offset_of!(TickTrade, is_buyer_maker),
        offset_of!(TickTrade, symbol),
    ];
    assert!(
        quote.is_sorted() && quote[0] == 0,
        "TickQuote's fields in declaration order: {quote:?}"
    );
    // The research study file's by-value boundary (`crate::study`): a pointer and a word each, so a field
    // added, removed or reordered moves the layout the study loader checks.
    use crate::study::{BytesAbi, SinkAbi, StudyHostAbi};
    let word = size_of::<usize>();
    for (name, size, align, offsets) in [
        (
            "BytesAbi",
            size_of::<BytesAbi>(),
            align_of::<BytesAbi>(),
            [offset_of!(BytesAbi, ptr), offset_of!(BytesAbi, len)],
        ),
        (
            "SinkAbi",
            size_of::<SinkAbi>(),
            align_of::<SinkAbi>(),
            [offset_of!(SinkAbi, sink), offset_of!(SinkAbi, write)],
        ),
        (
            "StudyHostAbi",
            size_of::<StudyHostAbi>(),
            align_of::<StudyHostAbi>(),
            [offset_of!(StudyHostAbi, host), offset_of!(StudyHostAbi, call)],
        ),
    ] {
        assert_eq!((size, align, offsets), (2 * word, word, [0, word]), "{name}");
    }
    assert!(
        trade.is_sorted() && trade[0] == 0,
        "TickTrade's fields in declaration order: {trade:?}"
    );
    // `ABI_VERSION` 15: the rows a file lends to `place_group`, `repr(C)` in declaration order.
    let leg = [
        offset_of!(OrderListLegAbi, tag),
        offset_of!(OrderListLegAbi, side),
        offset_of!(OrderListLegAbi, qty),
        offset_of!(OrderListLegAbi, order_type),
        offset_of!(OrderListLegAbi, order_type_word),
        offset_of!(OrderListLegAbi, price),
        offset_of!(OrderListLegAbi, trigger_price),
        offset_of!(OrderListLegAbi, trail),
        offset_of!(OrderListLegAbi, reduce_only),
        offset_of!(OrderListLegAbi, time_in_force),
        offset_of!(OrderListLegAbi, parent),
        offset_of!(OrderListLegAbi, contingency),
        offset_of!(OrderListLegAbi, linked),
        offset_of!(OrderListLegAbi, order_client_id),
    ];
    assert!(
        leg.is_sorted() && leg[0] == 0,
        "OrderListLegAbi's fields in declaration order: {leg:?}"
    );
    assert_eq!((size_of::<OrderListLegAbi>(), align_of::<OrderListLegAbi>()), (176, 8));
    assert_ne!(layout(), 0, "0 is a caught panic's answer");
}

// `abi_version_is_nonzero` used to live here as a `#[test]` asserting `ABI_VERSION > 0`.
// `clippy::assertions_on_constants` (a `-D warnings` merge-gate lint) correctly flags an
// `assert!` whose condition is knowable at compile time — the same claim now lives as a
// `const _: () = assert!(..)` beside `ABI_VERSION`'s own declaration, which is CHECKED ON
// EVERY BUILD rather than only under `cargo test`, so nothing was lost by deleting the test.

// ---- ABI_VERSION 3: the tier-2 mirrors ----

/// Every field of every tier-2 mirror must survive the round trip. One test over all three
/// rather than near-identical ones: the failure they guard against is a field FORGOTTEN
/// in `of` or in `to_*`, and a forgotten field is equally invisible in whichever mirror it
/// sits in. Every value below is distinct and non-default, so a field left at `Default` or
/// copied from its neighbour fails rather than passing on a coincidence. The two tick mirrors
/// went at `ABI_VERSION` 15: the tick hooks lend the host's own tick.
#[test]
fn the_tier_two_mirrors_round_trip_every_field() {
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

/// An EMPTY symbol must come back empty, not as a panic and not as a fabricated value. A fill
/// carries `symbol: String`, and "" is the documented single-symbol-path value — so this is the
/// ordinary case on those paths, not an edge one. It exercises [`read_borrowed_str`]'s
/// zero-length early return, which is the branch that keeps a null pointer from ever reaching
/// `from_raw_parts`.
#[test]
fn an_empty_symbol_crosses_as_empty_rather_than_panicking() {
    let f = vike_model::Fill {
        side: 1,
        size: 1.0,
        price: 2.0,
        fee: 0.0,
        timestamp: 1,
        is_maker: false,
        symbol: String::new(),
        tag: None,
    };
    assert_eq!(FillAbi::of(&f).to_fill().symbol, "");
}

/// Both former copies' panic text, byte for byte (`expect`'s `"{msg}: {err:?}"` shape): this side
/// says "borrowed string", the host's `read_str` says "symbol". A valid pair reads back unchanged.
#[test]
fn read_borrowed_str_reads_valid_bytes_and_names_what_it_refused() {
    let s = "BTCUSDT";
    assert_eq!(read_borrowed_str(s.as_ptr(), s.len(), "symbol"), "BTCUSDT");
    let bad = [0xffu8, 0xfe];
    let utf8_err = String::from_utf8(bad.to_vec()).unwrap_err().utf8_error();
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

/// Every leg an order list can hold crosses the lent rows as itself (`ABI_VERSION` 15): every order type
/// (an unrecognized word verbatim), every lifetime and contingency, a parent and none, links, an id and
/// none and an empty one, and every float by its BITS (a NaN's payload, a negative zero, the infinities),
/// so the host refuses a NaN price in the words a native order meets. A code this side does not know is
/// refused naming its field, never guessed.
#[test]
fn an_order_list_crosses_its_lent_rows_bit_for_bit() {
    use vike_model::{ContingencyType, OrderListLeg, OrderType, TimeInForce};
    let odd_nan = f64::from_bits(0x7ff8_0000_dead_beef);
    let base = OrderListLeg {
        side: -1,
        qty: odd_nan,
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
    let mut legs: Vec<(String, OrderListLeg)> = Vec::new();
    let kinds =
        OrderType::KNOWN.iter().cloned().chain([OrderType::Unrecognized("lmt".to_string())]);
    for (i, order_type) in kinds.enumerate() {
        legs.push((format!("k#{i}"), OrderListLeg { order_type, qty: -0.0, ..base.clone() }));
    }
    for (i, time_in_force) in TimeInForce::ALL.iter().copied().enumerate() {
        legs.push((format!("t#{i}"), OrderListLeg { time_in_force, price: None, ..base.clone() }));
    }
    for (i, contingency) in
        [ContingencyType::Oto, ContingencyType::Oco, ContingencyType::Ouo].into_iter().enumerate()
    {
        let linked = vec![0, i, usize::MAX];
        let id = Some(if i == 0 { String::new() } else { format!("id-{i}") });
        let leg = OrderListLeg {
            contingency: Some(contingency),
            parent: Some(i),
            linked,
            order_client_id: id,
            ..base.clone()
        };
        legs.push((String::new(), leg));
    }
    let rows: Vec<OrderListLegAbi> =
        legs.iter().map(|(tag, leg)| OrderListLegAbi::of(tag, leg)).collect();
    let back = order_list_of(SliceAbi::of(&rows)).expect("its own rows decode");
    let bits = |l: &OrderListLeg| {
        let opt = |x: Option<f64>| x.map(f64::to_bits);
        (l.qty.to_bits(), opt(l.price), opt(l.trigger_price), opt(l.trail))
    };
    assert_eq!(format!("{back:?}"), format!("{legs:?}"));
    for ((tag, leg), (want_tag, want)) in back.iter().zip(&legs) {
        assert_eq!((tag, bits(leg)), (want_tag, bits(want)));
        assert_eq!(
            (&leg.order_type, leg.time_in_force, leg.parent, leg.contingency, &leg.linked),
            (&want.order_type, want.time_in_force, want.parent, want.contingency, &want.linked)
        );
        assert_eq!(leg.order_client_id, want.order_client_id);
    }
    assert_eq!(back[10].1.order_type, OrderType::Unrecognized("lmt".to_string()), "verbatim");
    assert_eq!(order_list_of(SliceAbi::empty()).expect("no legs"), Vec::new());
    let words = vike_model::order_terms_refusal(
        &OrderType::Limit,
        back[1].1.price,
        None,
        None,
        None,
        None,
        None,
    );
    assert_eq!(words.as_deref(), Some("the price NaN is not a finite number"));

    let row = OrderListLegAbi::of("z#1", &base);
    for (bad, field) in [
        (OrderListLegAbi { order_type: 10, ..row }, "order type code 10"),
        (OrderListLegAbi { time_in_force: 8, ..row }, "time-in-force code 8"),
        (OrderListLegAbi { contingency: OptionAbi::new(Some(3), 0), ..row }, "contingency code 3"),
    ] {
        let refused = order_list_of(SliceAbi::of(&[row, bad])).expect_err("refused");
        assert!(refused.starts_with("leg 1: ") && refused.contains(field), "{refused}");
    }
}
