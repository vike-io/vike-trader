use super::*;

/// A `CBar` must be plain data: no padding surprises, no Rust types.
/// Pinned because a field added without `#[repr(C)]` discipline silently
/// changes the layout both sides agreed on.
#[test]
fn cbar_layout_is_pinned() {
    assert_eq!(std::mem::size_of::<COptF64>(), 16);
    assert_eq!(std::mem::align_of::<CBar>(), 8);
    // ts + 5 f64 + 3 COptF64 + symbol ptr/len
    assert_eq!(std::mem::size_of::<CBar>(), 8 + 5 * 8 + 3 * 16 + 16);
}

// `abi_version_is_nonzero` used to live here as a `#[test]` asserting `ABI_VERSION > 0`.
// `clippy::assertions_on_constants` (a `-D warnings` merge-gate lint) correctly flags an
// `assert!` whose condition is knowable at compile time — the same claim now lives as a
// `const _: () = assert!(..)` beside `ABI_VERSION`'s own declaration, which is CHECKED ON
// EVERY BUILD rather than only under `cargo test`, so nothing was lost by deleting the test.

/// `from_bar` -> `to_bar` must be lossless: every optional survives, including a `None` one
/// (which must NOT come back as `Some(NaN)` — `COptF64`'s `present` flag, not the NaN sentinel
/// in its `value`, is what `Into<Option<f64>>` reads).
#[test]
fn cbar_round_trip_preserves_optionals_and_symbol() {
    let original = vike_marketdata::Bar {
        ts: 1_700_000_000_000,
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

    let c = CBar::from_bar(&original);
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
        ts: 1,
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

    let c = CBar::from_bar(&original);
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
    let q = vike_marketdata::QuoteTick {
        ts: 1_700_000_000_001,
        local_ts: 1_700_000_000_002,
        bid: 99.5,
        ask: 100.5,
        bid_size: 3.25,
        ask_size: 4.75,
        symbol: "BTCUSDT".to_string(),
    };
    assert_eq!(CQuoteTick::of(&q).to_quote_tick(), q);

    let t = vike_marketdata::TradeTick {
        ts: 1_700_000_000_003,
        local_ts: 1_700_000_000_004,
        price: 100.25,
        size: 0.5,
        is_buyer_maker: true,
        symbol: "ETHUSDT".to_string(),
    };
    assert_eq!(CTradeTick::of(&t).to_trade_tick(), t);

    let m = vike_model::MarkTick {
        symbol: "btcusdt".to_string(),
        price: 64_321.5,
        ts: 1_700_000_000_005,
    };
    assert_eq!(CMarkTick::of(&m).to_mark_tick(), m);

    let f = vike_model::Fill {
        side: -1,
        size: 2.5,
        price: 101.75,
        fee: 0.0625,
        ts: 1_700_000_000_006,
        is_maker: true,
        symbol: "SOLUSDT".to_string(),
    };
    assert_eq!(CFill::of(&f).to_fill(), f);

    let fl = vike_model::FlowToxicity { bid: 0.25, ask: 0.75, ts: 1_700_000_000_007 };
    assert_eq!(CFlowToxicity::of(fl).to_flow(), fl);
}

/// An EMPTY symbol must come back empty, not as a panic and not as a fabricated value. The
/// tick types carry `symbol: String` (not `Option<String>` like `Bar`), and "" is the
/// documented single-symbol-path value — so this is the ordinary case on those paths, not an
/// edge one. It exercises [`read_borrowed_str`]'s zero-length early return, which is the
/// branch that keeps a null pointer from ever reaching `from_raw_parts`.
#[test]
fn an_empty_symbol_crosses_as_empty_rather_than_panicking() {
    let q = vike_marketdata::QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    assert_eq!(CQuoteTick::of(&q).to_quote_tick().symbol, "");
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

/// ...and the whole `OrderLifecycle`, through the mirror, with the `tag` present and absent.
/// The absent case is the one that would silently pass on a null-pointer heuristic: `Some("")`
/// and `None` are different facts about an order, and only `COptStrRef::present` separates
/// them.
#[test]
fn an_order_lifecycle_round_trips_including_an_absent_tag() {
    let tagged = vike_model::OrderLifecycle {
        client_order_id: "vike-1".to_string(),
        tag: Some("bid-1".to_string()),
        kind: vike_model::OrderEventKind::Canceled { reason: "replaced".to_string() },
    };
    assert_eq!(COrderLifecycle::of(&tagged).to_order_lifecycle(), Some(tagged.clone()));

    let untagged = vike_model::OrderLifecycle {
        client_order_id: "vike-2".to_string(),
        tag: None,
        kind: vike_model::OrderEventKind::Accepted,
    };
    let back = COrderLifecycle::of(&untagged).to_order_lifecycle();
    assert_eq!(back, Some(untagged));

    let empty_tag = vike_model::OrderLifecycle {
        client_order_id: "vike-3".to_string(),
        tag: Some(String::new()),
        kind: vike_model::OrderEventKind::Filled,
    };
    assert_eq!(
        COrderLifecycle::of(&empty_tag).to_order_lifecycle(),
        Some(empty_tag),
        "an EMPTY tag is not an ABSENT tag — see COptStrRef's own doc"
    );
}

/// A `COrderLifecycle` carrying a kind code this host does not know is refused whole, rather
/// than delivered with a guessed kind.
#[test]
fn an_unknown_lifecycle_kind_refuses_the_whole_event() {
    let coid = "vike-9";
    let ev = COrderLifecycle {
        client_order_id: CStrRef::of(coid),
        tag: COptStrRef::of(None),
        kind: 99,
        reason: CStrRef::empty(),
    };
    assert!(ev.to_order_lifecycle().is_none());
}
