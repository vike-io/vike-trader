use super::*;
use crate::test_support::observe_frame::full_wire_snapshot;
use crate::wire::{
    ArmingNotRunningViewWire, CoreSnapshotWire, OrderHeldViewWire, OrderViewWire, VenueBlockWire,
};
use vike_model::OrderStatus;

/// Every `OrderStatus` variant. The no-wildcard `match` below is the COMPILE-TIME
/// exhaustiveness pin: adding a variant to `vike_model::OrderStatus` fails to compile right
/// here until it is added to BOTH the list and the arm — which then feeds the round-trip
/// assertions below. This is the audit's ask: a new variant can no longer silently fall
/// through `map_order_status`'s unknown arm and render as terminal `Rejected` in the observer
/// (the forward projection in vike-tradehub `publish.rs` renders `status.as_str()`; nodes built
/// before it rendered `format!("{:?}", status)`, so BOTH spellings are the wire contract).
fn all_order_statuses() -> Vec<OrderStatus> {
    use OrderStatus as S;
    let all = vec![
        S::Initialized,
        S::Submitted,
        S::Accepted,
        S::Triggered,
        S::PartiallyFilled,
        S::Filled,
        S::Canceled,
        S::Rejected,
        S::Denied,
        S::Expired,
        S::PendingCancel,
        S::Liquidated,
        S::Emulated,
        S::Released,
    ];
    for s in &all {
        match s {
            S::Initialized
            | S::Submitted
            | S::Accepted
            | S::Triggered
            | S::PartiallyFilled
            | S::Filled
            | S::Canceled
            | S::Rejected
            | S::Denied
            | S::Expired
            | S::PendingCancel
            | S::Liquidated
            | S::Emulated
            | S::Released => {}
        }
    }
    all
}

/// The OLDER nodes' spelling (`format!("{:?}", status)`, what vike-tradehub `publish.rs`'s
/// `project_order` sent before it sent `as_str`) round-trips IDENTICALLY for every variant — the
/// Debug-spelling fallback arm of `map_order_status`, kept for a desktop observing an older node.
#[test]
fn every_status_round_trips_through_the_debug_spelling() {
    for s in all_order_statuses() {
        assert_eq!(map_order_status(&format!("{s:?}")), s, "Debug spelling of {s:?}");
    }
}

/// The canonical SCREAMING_SNAKE `as_str` spelling (`OrderStatus::parse`'s vocabulary, what the
/// node sends) decodes for every variant — the first branch of `map_order_status`.
#[test]
fn every_status_round_trips_through_the_screaming_snake_spelling() {
    for s in all_order_statuses() {
        assert_eq!(map_order_status(s.as_str()), s, "as_str spelling of {s:?}");
    }
}

/// An unknown wire status decodes to terminal `Rejected` — never a live/actionable state.
/// Wrong-case spellings of real variants are unknown too (both real vocabularies are
/// exact-match).
#[test]
fn unknown_status_falls_back_to_rejected() {
    assert_eq!(map_order_status("BOGUS_STATUS"), OrderStatus::Rejected);
    assert_eq!(map_order_status(""), OrderStatus::Rejected);
    assert_eq!(map_order_status("accepted"), OrderStatus::Rejected);
    assert_eq!(map_order_status("FILLED "), OrderStatus::Rejected);
}

/// The shared reading every surface calls: both spellings read as the status for every variant (an older node's
/// `Filled` is `Filled`, not unknown), and a word neither spells is `None`, never a guess, so each surface keeps
/// its own answer for it (the desktop's `Rejected` above).
#[test]
fn the_shared_reading_takes_both_spellings_and_names_no_unknown_word() {
    for s in all_order_statuses() {
        assert_eq!(order_status_of(s.as_str()), Some(s), "as_str spelling of {s:?}");
        assert_eq!(order_status_of(&format!("{s:?}")), Some(s), "Debug spelling of {s:?}");
    }
    for unknown in ["BOGUS_STATUS", "", "accepted", "FILLED "] {
        assert_eq!(order_status_of(unknown), None, "{unknown:?}");
    }
}

/// The top-level scalars, order fields, position fields (with the documented off-path
/// defaults), and held-exit fields all map through `wire_to_core`; the observer-only fields
/// (`marks`/`mounts`/`recon`/counters) stay empty/zero.
#[test]
fn wire_to_core_maps_orders_positions_and_held_exits() {
    // A refused order's words and its catalog id cross the bridge together.
    let mut refused = full_wire_snapshot();
    refused.orders[0].reason = Some("too small".into());
    refused.orders[0].refusal_id = Some("order.venue.rejected".into());
    let order = &wire_to_core(&refused).orders[0];
    assert_eq!(
        (order.reason.as_deref(), order.refusal_id.as_deref()),
        (Some("too small"), Some("order.venue.rejected"))
    );

    let snap = wire_to_core(&full_wire_snapshot());

    assert_eq!(snap.sequence, 42);
    assert_eq!(snap.venue, "binance");
    assert_eq!(snap.symbol, "BTCUSDT");
    assert_eq!(snap.trading_state, vike_model::TradingState::Reducing);
    assert_eq!(snap.balance.to_bits(), 10_000.0_f64.to_bits());
    assert_eq!(snap.balance_mode, vike_execution::BalanceMode::Delta);
    let recent: Vec<&str> = snap.recent_events.iter().map(|s| &**s).collect();
    assert_eq!(recent, vec!["OrderAccepted c-1"]);
    assert_eq!(snap.fault.as_deref(), Some("boom"));

    // Whole-struct comparisons (derived PartialEq) pin every carried field at once.
    assert_eq!(
        snap.orders,
        vec![std::sync::Arc::new(vike_execution::OrderView {
            data_delay: None,
            order_client_id: "c-1".into(),
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: vike_model::OrderType::Limit,
            price: Some(59_000.0),
            trigger_price: None,
            status: OrderStatus::Accepted,
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.25,
            average_fill_price: 59_100.0,
            refusal_id: None,
            reason: None,
            pending_price: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
        })]
    );
    assert_eq!(
        snap.positions,
        vec![vike_execution::PositionView {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            size: 0.5,
            average_price: 60_000.0,
            unrealized: 2_345.67,
            mark_source: None, // off-path default (resolver-only)
            leverage: 5.0,
            liquidation_price: 48_000.0,
            margin_mode: vike_model::MarginMode::Cross, // off-path default
            isolated_margin: None,                      // off-path default
        }]
    );
    assert_eq!(
        snap.held_exits,
        vec![vike_execution::OrderHeldView {
            order_client_id: "c-2".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: vike_model::OrderType::Stop,
            price: None,
            trigger_price: Some(55_000.0),
            parent_order_id: Some("c-1".into()),
            account: None,
            release: None,
            waiting: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            display_quantity: None,
        }]
    );
    // An order held until its release maps back with its account and its release, the price condition on the
    // order's own symbol.
    let time_held_wire = || OrderHeldViewWire {
        order_client_id: "h-1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(90.0),
        trigger_price: None,
        parent_order_id: None,
        account: Some("ALT".into()),
        release: Some(crate::wire::OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(crate::wire::OrderReleasePriceWire::Above(95.0)),
            symbol: None,
            venue: None,
        }),
        waiting: None,
        release_venue: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };
    let time_held = map_held(&time_held_wire());
    assert_eq!(time_held.account.map(|l| l.to_string()).as_deref(), Some("ALT"));
    assert_eq!(
        time_held.release,
        Some(vike_model::OrderRelease {
            at: 4_097_217_600_000,
            price: Some(vike_model::OrderReleasePrice {
                symbol: "BTCUSDT".into(),
                level: 95.0,
                side: vike_model::OrderReleasePriceSide::Above,
                venue: None,
            }),
        })
    );
    // A condition the node says reads ANOTHER symbol keeps that symbol.
    let mut other = time_held_wire();
    if let Some(r) = other.release.as_mut() {
        r.symbol = Some("ETHUSDT".into());
    }
    let mapped = map_held(&other).release.and_then(|r| r.price).map(|c| c.symbol.to_string());
    assert_eq!(mapped.as_deref(), Some("ETHUSDT"));
    // A condition read on ANOTHER venue folds the row's `release_venue` reading back into the condition, and the
    // node's waiting words ride along; no reading is the order's own venue (the test above).
    let mut elsewhere = time_held_wire();
    elsewhere.release_venue = Some("bybit".into());
    elsewhere.waiting = Some("no price of BTCUSDT on bybit on this node yet".into());
    let held = map_held(&elsewhere);
    let venue = held.release.and_then(|r| r.price).and_then(|c| c.venue).map(|v| v.to_string());
    assert_eq!(venue.as_deref(), Some("bybit"));
    assert_eq!(held.waiting.as_deref(), Some("no price of BTCUSDT on bybit on this node yet"));

    // Observer-only voids: no local core state.
    assert!(snap.marks.is_empty());
    assert!(snap.mounts.is_empty());
    // The node's health counters cross field for field (each fixture value is distinct, so a swap shows)...
    let named = snap.counters.named();
    assert_eq!(named.map(|(_, value)| value), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10], "{named:?}");
    // ...and a node that predates the block reads as all zero here (only the frame tells "not said").
    let older = CoreSnapshotWire { counters: None, ..full_wire_snapshot() };
    assert_eq!(wire_to_core(&older).counters, vike_execution::CoreCounters::default());
    assert_eq!(snap.reconcile, vike_execution::ReconcileBlock::default());
    assert!(snap.reconcile_coin_deltas.is_empty());
    assert!(snap.core_held_stops.is_empty(), "a node with none armed lists none");
    assert!(snap.emulated_orders.is_empty(), "a node that emulates nothing lists nothing");
}

/// The node's ONE emulated list maps back field for field (decision 0275): the label as an account, the peg's
/// anchor word as the model's anchor, the kind as its word; a kind word this build does not know is still listed.
#[test]
fn wire_to_core_carries_the_nodes_emulated_orders() {
    let row = crate::wire::OrderEmulationViewWire {
        id: "c-77".into(),
        venue: "bybit".into(),
        account: Some("ALT".into()),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        kind: "chase".into(),
        reduce_only: false,
        trigger: None,
        price: None,
        trail: None,
        peg: Some(crate::wire::OrderPegWire { anchor: "mid".into(), offset: 0.5 }),
        display_quantity: None,
        filled: 0.4,
        average_price: 60_010.0,
        child: Some("c-77-3".into()),
        waiting: None,
        rearms: 1,
        held: false,
    };
    let mut wire = full_wire_snapshot();
    wire.emulated_orders = vec![
        row.clone(),
        crate::wire::OrderEmulationViewWire {
            kind: "a_kind_from_later".into(),
            account: None,
            ..row
        },
    ];

    let snap = wire_to_core(&wire);

    assert_eq!(
        snap.emulated_orders[0],
        vike_execution::OrderEmulationView {
            id: "c-77".into(),
            venue: "bybit".into(),
            account: Some(
                vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a label")
            ),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            kind: "chase",
            reduce_only: false,
            trigger: None,
            price: None,
            trail: None,
            peg: Some(vike_model::OrderPeg {
                anchor: vike_model::OrderPegAnchor::Mid,
                offset: 0.5
            }),
            display_quantity: None,
            filled: 0.4,
            average_price: 60_010.0,
            child: Some("c-77-3".into()),
            waiting: None,
            rearms: 1,
            held: false,
        }
    );
    assert_eq!(
        (snap.emulated_orders[1].kind, &snap.emulated_orders[1].account),
        ("emulated", &None)
    );
}

/// A resting order's attributes-wave readings map back field for field: its attributes, its percent trail, its
/// adjustment (each target) and the release it rests with.
#[test]
fn a_resting_orders_attributes_trail_percent_adjustment_and_release_map_back() {
    use crate::wire::OrderReleaseWire;
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    let mut wire = full_wire_snapshot().orders.remove(0);
    let attributes = OrderAttributes {
        all_or_none: false,
        sweep_to_fill: true,
        discretionary_amount: Some(0.25),
    };
    wire.attributes = Some(attributes);
    wire.trail_percent = Some(1.5);
    wire.release =
        Some(OrderReleaseWire { at: 4_097_217_600_000, price: None, symbol: None, venue: None });
    let order = map_order(&wire);
    assert_eq!(order.attributes, Some(attributes));
    assert_eq!(order.trail_percent, Some(1.5));
    assert_eq!(
        order.release,
        Some(vike_model::OrderRelease { at: 4_097_217_600_000, price: None })
    );
    assert_eq!(order.adjust, None, "no adjustment read, none mapped");
    for to in [
        OrderStopAdjustTo::Stop { stop: 57_000.0 },
        OrderStopAdjustTo::StopLimit { stop: 57_000.0, limit: 56_900.0 },
        OrderStopAdjustTo::Trailing { stop: 57_000.0, trail: Some(100.0), trail_percent: None },
    ] {
        wire.adjust = Some(OrderStopAdjust { at: 60_000.0, to });
        assert_eq!(map_order(&wire).adjust, Some(OrderStopAdjust { at: 60_000.0, to }));
    }
}

/// A HELD row's terms map back as a resting order's do (its attributes, its percent trail, its adjustment and its
/// display), and a resting order's display maps back too (zero: a HIDDEN order). A row without them maps to `None`
/// (`wire_to_core_maps_orders_positions_and_held_exits`' whole-struct comparisons).
#[test]
fn a_held_rows_terms_and_a_resting_orders_display_map_back() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    let mut wire = full_wire_snapshot();
    let mut held = wire.held_exits.remove(0);
    held.attributes = Some(OrderAttributes { all_or_none: true, ..Default::default() });
    held.trail_percent = Some(1.5);
    held.adjust = Some(OrderStopAdjust { at: 120.0, to: OrderStopAdjustTo::Stop { stop: 110.0 } });
    held.display_quantity = Some(0.0);
    let mapped = map_held(&held);
    let attributes = OrderAttributes { all_or_none: true, ..Default::default() };
    assert_eq!(mapped.attributes, Some(attributes));
    assert_eq!(mapped.trail_percent, Some(1.5));
    assert_eq!(
        mapped.adjust,
        Some(OrderStopAdjust { at: 120.0, to: OrderStopAdjustTo::Stop { stop: 110.0 } })
    );
    assert_eq!(mapped.display_quantity, Some(0.0));

    let mut order = wire.orders.remove(0);
    order.display_quantity = Some(0.0);
    assert_eq!(map_order(&order).display_quantity, Some(0.0));
    order.display_quantity = Some(2.0);
    assert_eq!(map_order(&order).display_quantity, Some(2.0), "an iceberg's slice");
}

/// The node's armed core-held stops reach the observer's snapshot whole (the wire's `account` is the
/// row's `route_key`).
#[test]
fn wire_to_core_carries_the_nodes_core_held_stops() {
    let mut wire = full_wire_snapshot();
    wire.core_held_stops = vec![crate::wire::OrderEmulationStopViewWire {
        arm_id: "s0a1".into(),
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        account: "polymarket#ALT".into(),
        side: -1,
        qty: 5.0,
        trigger_price: 0.4,
        held: true,
        rearms: 2,
    }];

    let snap = wire_to_core(&wire);

    assert_eq!(
        snap.core_held_stops,
        vec![vike_execution::OrderEmulationStopView {
            arm_id: "s0a1".into(),
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            route_key: "polymarket#ALT".into(),
            side: -1,
            qty: 5.0,
            trigger_price: 0.4,
            held: true,
            rearms: 2,
        }]
    );
}

/// `build_portfolio`'s primary-vs-summed derivation: the scalar fields MIRROR `venues[0]`
/// (never summed — exactly as `vike_core::snapshot::build` populates them), while
/// `margin_used_total`/`missing_prices_total` ARE cross-venue sums and `equity_total` is
/// carried from the wire aggregate.
#[test]
fn build_portfolio_mirrors_primary_and_sums_the_totals() {
    let wire = full_wire_snapshot();
    let p = build_portfolio(&wire);

    assert_eq!(p.equity.to_bits(), 12_345.67_f64.to_bits(), "mirrors venues[0], not summed");
    assert_eq!(p.equity_total.to_bits(), 12_400.0_f64.to_bits(), "the wire aggregate");
    assert_eq!(p.realized_pnl.to_bits(), 250.5_f64.to_bits());
    assert_eq!(p.fees_paid.to_bits(), 3.25_f64.to_bits());
    assert_eq!(p.funding_paid.to_bits(), (-1.5_f64).to_bits());
    assert_eq!(p.margin_used_total.to_bits(), 900.0_f64.to_bits(), "500 + 400 summed");
    assert_eq!(p.missing_prices_total, 3, "1 + 2 summed");
    assert!(p.balances_by_asset.is_empty(), "not on the wire");
    assert_eq!(p.venues.len(), 2);
}

/// An empty wire snapshot (no venues) yields the 0.0 primary-mirror fields — the `unwrap_or`
/// arms, never a panic. The `to_bits` comparisons pin POSITIVE zero: `margin_used_total`
/// must come from the naive fold (std's float `Sum` empty identity is `-0.0`, which would
/// diverge bitwise from `CoreSnapshot::empty`'s `Portfolio::default()` placeholder).
#[test]
fn build_portfolio_with_no_venues_zeroes_the_primary_mirror() {
    let p = build_portfolio(&CoreSnapshotWire::empty());
    assert_eq!(p.equity.to_bits(), 0.0_f64.to_bits());
    assert_eq!(p.realized_pnl.to_bits(), 0.0_f64.to_bits());
    assert_eq!(p.margin_used_total.to_bits(), 0.0_f64.to_bits());
    assert_eq!(p.missing_prices_total, 0);
    assert!(p.venues.is_empty());
}

/// ⚠ **The remote GUI adds one currency only** (decision 0183, item 13): a wire frame whose second block
/// is in EUR gives a portfolio whose `margin_used_total` is the PRIMARY group's, whose blocks carry
/// their group, and whose `ledger_totals` keeps the EUR figure on its own row.
#[test]
fn build_portfolio_adds_the_primary_currency_group_only() {
    let mut wire = full_wire_snapshot();
    wire.venues[1].ledger_currency = "EUR".into();
    let p = build_portfolio(&wire);

    assert_eq!(
        p.margin_used_total.to_bits(),
        500.0_f64.to_bits(),
        "the dollar block's 500, not +400"
    );
    assert_eq!(p.venues[0].ledger_currency, "");
    assert_eq!(p.venues[1].ledger_currency, "EUR");
    assert!(p.is_multi_currency());
    let rows = p.ledger_totals();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].margin_used.to_bits(), p.margin_used_total.to_bits());
    assert_eq!(rows[1].margin_used.to_bits(), 400.0_f64.to_bits(), "the EUR block's own 400");
    // The count of unpriced positions is not money: it is the whole node's.
    assert_eq!(p.missing_prices_total, 3);
}

/// **The node's word that a block's currency is only an assumed USD reaches the observer's block verbatim**
/// (decision 0189, A52), per block; a frame that carries none reads `false` (an older node).
#[test]
fn a_blocks_assumed_currency_flag_reaches_the_observers_portfolio_verbatim() {
    let mut wire = full_wire_snapshot();
    assert!(
        build_portfolio(&wire).venues.iter().all(|v| !v.currency_assumed),
        "an older node says none"
    );
    wire.venues[1].currency_assumed = true;
    let p = build_portfolio(&wire);
    assert!(!p.venues[0].currency_assumed && p.venues[1].currency_assumed);
}

/// `map_venue_block` recomputes `margin_ratio` from the wired `margin_used`/`equity` (the wire
/// does not carry it), guards the `equity <= 0` arm to 0.0, and defaults the GUI-irrelevant
/// core internals (`balance_mode` Delta / `fee_schedule` None / empty 1.0-default multiplier
/// grid).
#[test]
fn map_venue_block_recomputes_margin_ratio_and_defaults_the_internals() {
    let wire = full_wire_snapshot();
    let primary = map_venue_block(&wire.venues[0]);
    assert_eq!(
        primary.margin_ratio.to_bits(),
        (500.0_f64 / 12_345.67_f64).to_bits(),
        "recomputed margin_used / equity"
    );
    assert_eq!(primary.balance_mode, vike_execution::BalanceMode::Delta);
    assert_eq!(primary.fee_schedule, None);
    assert!(primary.contract_values.is_empty());
    assert_eq!(primary.contract_value_default, vike_model::ContractValue::default());
    assert_eq!(primary.trading_state, vike_model::TradingState::Reducing);
    assert_eq!(primary.positions.len(), 1);

    let zero_equity = map_venue_block(&wire.venues[1]);
    assert_eq!(
        zero_equity.margin_ratio.to_bits(),
        0.0_f64.to_bits(),
        "equity <= 0 guards the division to 0.0"
    );
    assert_eq!(zero_equity.trading_state, vike_model::TradingState::Halted);
}

/// The bridge keeps what the node said about each account: its label, its route key, the symbols
/// it trades and its tier (Trade window spec §4.3). A second account of one venue stays a second
/// account on the desktop.
#[test]
fn map_venue_block_keeps_the_account_its_symbols_and_its_tier() {
    let mut w = full_wire_snapshot().venues.remove(0);
    w.account = Some("SUB".into());
    w.route_key = "binance#SUB".into();
    w.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    w.tier = Some("live".into());
    let vb = map_venue_block(&w);
    assert_eq!(vb.account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
    assert_eq!(vb.route_key, "binance#SUB");
    assert_eq!(vb.symbol, "BTCUSDT");
    assert_eq!(vb.extra_symbols, vec!["ETHUSDT".to_string()]);
    assert_eq!(vb.tier, Some(vike_model::AccountTier::Live));
    assert!(vb.trades("ETHUSDT"));
}

/// An older node says none of it: the block names no account, a bare route key, no symbols (no
/// primary and no extras) and no tier ("not said"), and nothing panics.
#[test]
fn map_venue_block_reads_an_older_node_as_not_said() {
    let mut w = full_wire_snapshot().venues.remove(0);
    w.account = None;
    w.route_key = String::new();
    w.symbols = Vec::new();
    w.tier = None;
    let vb = map_venue_block(&w);
    assert!(vb.account.is_none());
    assert_eq!(vb.route_key, vb.venue, "an empty wire key falls back to the venue");
    assert!(vb.symbol.is_empty() && vb.tier.is_none());
    assert!(vb.extra_symbols.is_empty());
    assert!(!vb.trades("BTCUSDT"));
}

/// The wire spellings of an account that must NOT come out as a labelled one, shared by the block
/// read and the order read so the two cannot drift apart:
/// - `DEFAULT`, the reserved word for the unlabelled account. `parse_wire_account` accepts it and
///   answers `AccountLabel::Default`; the bridge's `.filter(|l| !l.is_default())` is what turns that
///   into `None`, the convention `VenueBlock::account` and `OrderView::account` both state. Without
///   it a `Some(Default)` is a second spelling of the unlabelled account that no reader expects.
/// - the empty label, and a label one past `MAX_LABEL_LEN`, which `parse_wire_account` refuses.
/// - a label with characters no label has.
fn unlabelled_wire_spellings() -> Vec<String> {
    vec![
        "DEFAULT".to_string(),
        String::new(),
        "A".repeat(vike_model::accounts::account_keys::MAX_LABEL_LEN + 1),
        "not a label!".to_string(),
    ]
}

/// A spelling that is not a labelled account cannot become one: a malformed label and the reserved
/// word for the default account both read as the default (unlabelled) account, `None`.
#[test]
fn map_venue_block_drops_a_malformed_label() {
    let mut w = full_wire_snapshot().venues.remove(0);
    for text in unlabelled_wire_spellings() {
        w.account = Some(text.clone());
        assert!(map_venue_block(&w).account.is_none(), "{text:?} must read as the default account");
    }
}

/// An order keeps the account it rests in, an unlabelled one stays unlabelled, and a spelling that
/// is not a labelled account (the ones the block read drops) maps to `None`, the only spelling this
/// type has for it. That `None` is never ATTRIBUTED as the default account's: see
/// `an_order_whose_label_this_client_cannot_read_is_attributed_to_no_window`.
#[test]
fn map_order_keeps_the_account() {
    let mut o = full_wire_snapshot().orders.remove(0);
    o.account = Some("SUB".into());
    assert_eq!(map_order(&o).account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
    o.account = None;
    assert!(map_order(&o).account.is_none());
    for text in unlabelled_wire_spellings() {
        o.account = Some(text.clone());
        assert!(map_order(&o).account.is_none(), "{text:?} names no labelled account");
    }
}

/// Decision 0140: the node's delay word reads back as the label the order row shows; no word, and a
/// word this client cannot name (a newer node's), read as no label — never a guess. And the round
/// trip through the wire's JSON keeps it.
#[test]
fn map_order_reads_the_delay_word_back_and_an_unknown_one_as_no_label() {
    use vike_model::StreamDelay;
    let mut o = full_wire_snapshot().orders.remove(0);
    assert_eq!(map_order(&o).data_delay, None, "an unlabelled order (an old node) has no label");
    for (word, label) in [
        ("delayed", StreamDelay::DELAYED),
        ("frozen", StreamDelay::Frozen),
        ("real-time", StreamDelay::RealTime),
    ] {
        o.data_delay = Some(word.into());
        let back: OrderViewWire =
            serde_json::from_str(&serde_json::to_string(&o).expect("serializes")).expect("parses");
        assert_eq!(map_order(&back).data_delay, Some(label), "{word}");
    }
    o.data_delay = Some("stale-ish".into());
    assert_eq!(map_order(&o).data_delay, None);
}

/// ⚠ **`OrderView::account == None` means "the venue's default account", and the type cannot say
/// "unknown".** A node that predates the order's `account` key sends a labelled order with no
/// account, and it maps to `None` exactly as a default-account order does. The only tell is the
/// node's venue blocks: `VenueBlockWire::tier` ships only in node releases that also send the
/// order's `account`, so a node whose blocks carry a tier is one that names its orders' accounts,
/// and a node whose blocks carry none is not. The tell is the tier's PRESENCE, not its value:
/// `paper` counts like the other two.
#[test]
fn map_order_none_means_default_and_a_blocks_tier_is_the_tell() {
    // A node that predates both: its block says no tier, and its order names no account.
    let mut older = full_wire_snapshot();
    older.venues[0].tier = None;
    older.orders[0].account = None;
    let older = wire_to_core(&older);
    assert!(older.portfolio.venues[0].tier.is_none(), "an older node's block says no tier");
    assert!(older.orders[0].account.is_none());

    // A node that names its orders' accounts: its block carries a tier, whichever one, and its
    // order names SUB. Every tier word is read as itself, so a real-money account is never read as
    // a safer one nor a demo account as a real-money one.
    for core in vike_model::AccountTier::ALL {
        let mut newer = full_wire_snapshot();
        newer.venues[0].tier = Some(core.as_str().into());
        newer.orders[0].account = Some("SUB".into());
        let newer = wire_to_core(&newer);
        assert_eq!(newer.portfolio.venues[0].tier, Some(core));
        assert_eq!(
            newer.orders[0].account.as_ref().map(ToString::to_string).as_deref(),
            Some("SUB"),
            "an order on a node whose blocks say {core:?} keeps its account"
        );
    }
}

/// A tier word this build does not know is "not said" (`MODE ?`), never PAPER; the strict parser
/// is the one edge where the word becomes a tier (decision 0129).
#[test]
fn a_tier_word_this_build_does_not_know_is_not_said() {
    assert_eq!(map_tier(Some("live")), Some(vike_model::AccountTier::Live));
    assert_eq!(map_tier(None), None);
    for word in ["staging", "", "sim", "mainnet"] {
        assert_eq!(map_tier(Some(word)), None, "{word:?}");
    }
}

/// Decision 0156: the accounts the node says it does NOT run cross the bridge with their route
/// key, their row's tier and the node's reason word, so the Trade window can say why one is not
/// running. An older node sends none: the list stays empty.
#[test]
fn the_accounts_the_node_does_not_run_cross_the_bridge_with_their_reason() {
    let mut w = full_wire_snapshot();
    assert!(wire_to_core(&w).not_running.is_empty(), "CONTROL: a node that names none");
    // `adapter failed` covers a held lease, a version skew and a refused key alike: what lies behind
    // it rides with the reason, so every surface that says the reason says it.
    let adapter = ArmingNotRunningViewWire {
        venue: "okx".into(),
        account: None,
        route_key: "okx".into(),
        tier: None,
        reason: "adapter failed".into(),
        detail: Some("another client holds the lease".into()),
        block_word: None,
        refusal_id: None,
    };
    assert_eq!(map_not_running(&adapter).reason, "adapter failed: another client holds the lease");
    // The block crosses as the wire read it (a word this build does not know is `AdapterFailed` there, never a
    // block that runs: `wire_tests.rs`'s `the_block_word_crosses_as_the_blocks_key_byte_for_byte`), and an older
    // node's row, with none, names none.
    let keyed = |block: Option<vike_model::ArmingBlock>| {
        map_not_running(&ArmingNotRunningViewWire { block_word: block, ..adapter.clone() }).block
    };
    let inactive = Some(vike_model::ArmingBlock::AccountInactive);
    assert_eq!(keyed(inactive), inactive);
    assert_eq!(keyed(None), None);
    w.not_running = vec![
        ArmingNotRunningViewWire {
            venue: "okx".into(),
            account: None,
            route_key: "okx".into(),
            tier: Some("live".into()),
            reason: "no credentials".into(),
            detail: None,
            block_word: None,
            refusal_id: None,
        },
        ArmingNotRunningViewWire {
            venue: "binance".into(),
            account: Some("SUB".into()),
            route_key: "binance#SUB".into(),
            tier: Some("demo".into()),
            reason: "flags.tradehub_live off".into(),
            detail: None,
            block_word: None,
            refusal_id: None,
        },
    ];
    let snap = wire_to_core(&w);
    let seen: Vec<_> = snap
        .not_running
        .iter()
        .map(|n| {
            (n.account.venue(), n.account.label_text(), n.route_key(), n.tier, n.reason.as_str())
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("okx", None, "okx".to_string(), Some(vike_model::AccountTier::Live), "no credentials"),
            (
                "binance",
                Some("SUB"),
                "binance#SUB".to_string(),
                Some(vike_model::AccountTier::Demo),
                "flags.tradehub_live off",
            ),
        ]
    );
}

/// **A venue this client's roster does not have is kept, never dropped** (owner ruling 2026-11-12, variant 2B): a
/// newer node's `kalshi#main` crosses the bridge as the account it names, as received, under the route key it
/// carried, so the Account window still lists it (as an unknown venue).
#[test]
fn a_not_running_row_of_a_venue_this_client_does_not_know_is_kept_as_received() {
    let row = ArmingNotRunningViewWire {
        venue: "kalshi".into(),
        account: Some("main".into()),
        route_key: "kalshi#main".into(),
        tier: Some("live".into()),
        reason: "no credentials".into(),
        block_word: None,
        detail: None,
        refusal_id: None,
    };
    let n = map_not_running(&row);
    assert_eq!(
        n.account,
        vike_model::accounts::account_keys::AccountNamed::Unknown {
            venue: "kalshi".into(),
            label: Some("main".into()),
        }
    );
    assert_eq!(n.route_key(), row.route_key);
    assert_eq!(
        (n.tier, n.reason.as_str()),
        (Some(vike_model::AccountTier::Live), "no credentials")
    );
}

/// The bounded wire bar tail is rebuilt into the core's `(venue, symbol, interval)` bar cache:
/// closed bars + the forming candle, with the symbol stamped and the wire-absent `Bar` fields
/// (`funding`/`bid`/`ask`) `None`.
#[test]
fn wire_to_core_rebuilds_the_bar_cache() {
    let snap = wire_to_core(&full_wire_snapshot());
    let key = ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string());
    let series = snap.bars.get(&key).expect("wire series lands under its (v, s, i) key");
    assert_eq!(
        *series.closed,
        vec![vike_marketdata::Bar {
            timestamp: 1_000,
            open: 60_000.0,
            high: 60_100.0,
            low: 59_900.0,
            close: 60_050.0,
            volume: 12.5,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".into()),
        }]
    );
    assert_eq!(
        series.forming,
        Some(vike_marketdata::Bar {
            timestamp: 61_000,
            open: 60_050.0,
            high: 60_080.0,
            low: 60_020.0,
            close: 60_070.0,
            volume: 3.2,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".into()),
        })
    );
}

/// A position's side word reads back as its leg, and a word outside the three is the one-way `Both` leg WITH the
/// operator's warn line it always had (level, words and field); the three known words warn nothing.
#[test]
fn an_unknown_position_side_reads_as_both_and_warns_as_it_always_did() {
    use vike_model::events::PositionSide;
    let mut p = full_wire_snapshot().positions.remove(0);
    for (word, side) in
        [("BOTH", PositionSide::Both), ("LONG", PositionSide::Long), ("SHORT", PositionSide::Short)]
    {
        p.position_side = word.into();
        let (mapped, logs) = vike_log::capture::captured(|| map_position(&p));
        assert_eq!(mapped.position_side, side, "{word}");
        assert!(logs.is_empty(), "{word} warns nothing: {logs:?}");
    }
    p.position_side = "SIDEWAYS".into();
    let (mapped, logs) = vike_log::capture::captured(|| map_position(&p));
    assert_eq!(mapped.position_side, PositionSide::Both, "shown as the one-way leg, never dropped");
    assert_eq!(mapped.size, p.size, "with its signed size");
    let [warned] = logs.as_slice() else { panic!("one warn line: {logs:?}") };
    assert_eq!(warned.level, tracing::Level::WARN);
    assert_eq!(
        warned.message,
        "observe: unknown wire position side; showing it as the one-way BOTH leg"
    );
    assert_eq!(warned.field("position_side"), Some("SIDEWAYS"));
}

/// The node's refusal reason reads back onto the core `OrderView` (the desktop's order rows), through
/// the wire's JSON; a node that sent none reads as none.
#[test]
fn map_order_reads_the_refusal_reason_back() {
    let mut o = full_wire_snapshot().orders.remove(0);
    assert_eq!(map_order(&o).reason, None, "an order a node did not refuse has no reason");
    o.reason = Some("INVALID_REQUEST: more digits than symbol allows".into());
    let back: OrderViewWire =
        serde_json::from_str(&serde_json::to_string(&o).expect("serializes")).expect("parses");
    assert_eq!(
        map_order(&back).reason.as_deref(),
        Some("INVALID_REQUEST: more digits than symbol allows")
    );
}

/// Decision 0170: the node's non-default contract values become the observer's grid, so the order
/// ticket sizes an inverse contract by its quantity; a kind this build does not know is dropped, not
/// guessed, and a node that sends none leaves every symbol at the default.
#[test]
fn the_nodes_contract_values_become_the_observers_grid() {
    use crate::wire::ContractValueWire;
    let mut block = full_wire_snapshot().venues.remove(0);
    block.contract_values = vec![
        ContractValueWire { symbol: "BTC-PERPETUAL".into(), kind: "inverse".into(), per_unit: 1.0 },
        ContractValueWire { symbol: "ESZ6".into(), kind: "linear".into(), per_unit: 50.0 },
        ContractValueWire { symbol: "X".into(), kind: "quanto".into(), per_unit: 3.0 },
        ContractValueWire { symbol: "Z".into(), kind: "inverse".into(), per_unit: 0.0 },
    ];
    let mapped = map_venue_block(&block);
    assert_eq!(mapped.contract_value_of("BTC-PERPETUAL"), vike_model::ContractValue::inverse(1.0));
    assert_eq!(mapped.contract_value_of("ESZ6"), vike_model::ContractValue::linear(50.0));
    assert_eq!(mapped.contract_value_of("X"), vike_model::ContractValue::default(), "unknown kind");
    assert_eq!(mapped.contract_value_of("BTCUSDT"), vike_model::ContractValue::default());
    assert_eq!(
        mapped.contract_value_of("Z"),
        vike_model::ContractValue::inverse(1.0),
        "a zero unit is folded to 1.0, the kind kept"
    );
}

/// **The node's conversion-rate table becomes the observer's** (decision 0183, phase 1): the rows, their age and
/// the currency they are in are read verbatim, a stale flag the node set stays set, and a rate that is not a
/// positive number is shown STALE rather than hidden (the observer folds a degenerate contract value to the
/// default; a rate it cannot trust must say so). A node that sends none leaves the table empty.
#[test]
fn the_nodes_rate_table_becomes_the_observers_and_a_bad_rate_reads_stale() {
    use crate::wire::QuoteRateViewWire;
    let mut block = full_wire_snapshot().venues.remove(0);
    let none = map_venue_block(&block);
    assert!(none.quote_rates.is_empty() && none.quote_rates_in.is_empty());
    assert!(!none.quote_rates_stale());

    block.quote_rates_in = "USD".into();
    block.quote_rates = vec![
        QuoteRateViewWire {
            currency: "GBP".into(),
            rate: 1.35,
            as_of_ms: 100,
            stale: false,
            held: false,
        },
        QuoteRateViewWire {
            currency: "JPY".into(),
            rate: 1.0 / 150.0,
            as_of_ms: 50,
            stale: true,
            held: true,
        },
        QuoteRateViewWire {
            currency: "CHF".into(),
            rate: 0.0,
            as_of_ms: 100,
            stale: false,
            held: false,
        },
        QuoteRateViewWire {
            currency: "CAD".into(),
            rate: f64::NAN,
            as_of_ms: 100,
            stale: false,
            held: false,
        },
    ];
    let mapped = map_venue_block(&block);
    assert_eq!(mapped.quote_rates_in, "USD");
    let got: Vec<(&str, bool, bool)> =
        mapped.quote_rates.iter().map(|r| (r.currency.as_str(), r.stale, r.held)).collect();
    assert_eq!(
        got,
        [("GBP", false, false), ("JPY", true, true), ("CHF", true, false), ("CAD", true, false)]
    );
    assert_eq!(mapped.quote_rates[0].rate, 1.35, "a good rate is read as sent");
    assert!(mapped.quote_rates_stale());
}

/// The block's id plan crosses the bridge (decision 0270): the account code and a shape the model can
/// mint; a code or a shape it cannot read is "not said", never handed to `OrderClientIdPlan::new` to panic on.
#[test]
fn a_blocks_account_code_and_order_client_id_shape_cross_the_bridge_only_when_mintable() {
    use vike_model::orders::client_id::{
        OrderClientIdAccountCode, OrderClientIdPlan, OrderClientIdShape,
    };
    let mut block = full_wire_snapshot().venues[0].clone();
    let said = |b: &VenueBlockWire| {
        (map_venue_block(b).account_code, map_venue_block(b).order_client_id_shape)
    };
    assert_eq!(said(&block), (None, None), "an older node says neither");
    block.account_code = Some("07".into());
    let compact = OrderClientIdShape { account: false, u_width: 11, max_gen: 0, tag: false };
    block.order_client_id_shape = Some(compact);
    assert_eq!(said(&block), (Some(OrderClientIdAccountCode::of_id(7)), Some(compact)));
    let _ = OrderClientIdPlan::new(compact, OrderClientIdAccountCode::of_id(7));
    for bad in [
        OrderClientIdShape { account: true, ..compact },
        OrderClientIdShape { u_width: 5, ..compact },
        OrderClientIdShape { u_width: 13, ..compact },
        OrderClientIdShape { max_gen: 100, ..compact },
    ] {
        block.order_client_id_shape = Some(bad);
        assert_eq!(map_venue_block(&block).order_client_id_shape, None, "{bad:?}");
    }
    block.account_code = Some("7".into());
    assert_eq!(map_venue_block(&block).account_code, None, "not two base36 characters");
}

/// ⚠ **A position says which source priced it, and an older node's frame reads as before** (the fix of top layers
/// wave 3: every decoded position read as not priced, so the Account window's unpriced badge listed the priced
/// symbols too). The node's word comes back as the core's `PriceSource`; a frame with no key (an older node), a
/// word this build does not know and a value that is not a string all read as `None`, not priced, and none fails
/// the frame.
#[test]
fn a_position_keeps_the_source_that_priced_it_and_an_older_frame_reads_as_not_priced() {
    let wire = |mark_source: &str| -> crate::wire::PositionViewWire {
        let json = format!(
            r#"{{"venue":"bybit","symbol":"BTCUSDT","position_side":"BOTH","size":0.5,"average_price":100.0,
            "unrealized":2.5,"leverage":0.0,"liquidation_price":0.0{mark_source}}}"#
        );
        serde_json::from_str(&json).expect("a position frame decodes")
    };
    for (key, want) in [
        (r#","mark_source":"last_trade""#, Some(vike_execution::PriceSource::LastTrade)),
        (r#","mark_source":"mark""#, Some(vike_execution::PriceSource::Mark)),
        ("", None),
        (r#","mark_source":"index""#, None),
        (r#","mark_source":7"#, None),
        (r#","mark_source":null"#, None),
    ] {
        assert_eq!(map_position(&wire(key)).mark_source, want, "{key:?}");
    }
    let unsaid = wire("");
    assert_eq!(unsaid.mark_source, None);
    let bytes = serde_json::to_string(&unsaid).expect("encodes");
    assert!(!bytes.contains("mark_source"), "a position priced by nothing writes no key: {bytes}");
}
