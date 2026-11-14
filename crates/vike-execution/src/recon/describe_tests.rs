use super::*;

/// One of every [`ReconcileDivergence`] variant, all naming `BTCUSDT` where they have a symbol at all.
///
/// ⚠ A hand-written list, and its completeness is asserted below rather than assumed — the
/// thing that FORCES a new variant to be described is `describe`'s total match, which is a
/// compile error. This list only makes the assertions iterate, and
/// `the_fixture_covers_every_kind` is what stops it silently covering eight of nine.
fn every_variant() -> Vec<ReconcileDivergence> {
    let fill = vike_model::FillReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some("c1".into()),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        timestamp: 1,
    };
    let order = vike_model::OrderStatusReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some("c1".into()),
        order_type: vike_model::OrderType::Limit,
        status: vike_model::OrderStatus::Filled,
        side: 1,
        qty: 1.0,
        filled_qty: 1.0,
        average_price: 100.0,
        timestamp: 1,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    };
    let pos = vike_model::PositionStatusReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Long,
        qty: 1.0,
        average_price: 100.0,
        timestamp: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    vec![
        ReconcileDivergence::MissingFill(fill),
        ReconcileDivergence::MissingTerminal { order: order.clone() },
        ReconcileDivergence::OrphanLocalOrder { order_client_id: "c9".into() },
        ReconcileDivergence::UnknownOrder(order),
        ReconcileDivergence::PositionDrift { report: pos.clone(), local_quantity: 0.0 },
        ReconcileDivergence::PositionOnlyExternal(pos),
        ReconcileDivergence::OrphanLocalPosition {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Long,
            local_quantity: 1.0,
        },
        ReconcileDivergence::BalanceDrift {
            venue: "bybit".into(),
            asset: "USDT".into(),
            local: 1.0,
            venue_balance: 2.0,
            timestamp: 1,
        },
        ReconcileDivergence::JournalDivergence {
            detail: "a journal detail built elsewhere".into(),
            recover_order: None,
        },
        ReconcileDivergence::OrderTermsDrift {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            order_client_id: "c9".into(),
            price: Some((100.0, 101.0)),
            trigger_price: None,
            qty: None,
            timestamp: 1,
        },
    ]
}

/// The fixture covers EVERY kind — otherwise the two assertions below quietly test eight of nine.
#[test]
fn the_fixture_covers_every_kind() {
    let mut seen: Vec<String> = every_variant().iter().map(|d| format!("{:?}", d.kind())).collect();
    seen.sort();
    let all = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), all, "the fixture repeats a kind: {seen:?}");
    let mut every: Vec<String> =
        ReconcileDivergenceKind::ALL.iter().map(|k| format!("{k:?}")).collect();
    every.sort();
    assert_eq!(seen, every, "the fixture must carry exactly one divergence of every kind");
}

/// **A description must not be the kind's own name.**
///
/// The `format!("{:?}", d.kind())` fallback this replaced produced
/// `kind=PositionOnlyExternal detail=PositionOnlyExternal`. This asserts the two differ for
/// every variant, which is the one property the fallback could never have.
#[test]
fn no_variant_describes_itself_as_its_own_kind() {
    for d in every_variant() {
        let desc = d.describe();
        assert_ne!(
            desc,
            format!("{:?}", d.kind()),
            "{:?} describes itself as its own name — that is the fallback this replaced",
            d.kind()
        );
        assert!(!desc.is_empty(), "{:?} describes itself as nothing", d.kind());
    }
}

/// An `UnknownOrder` venue row (ACCEPTED, `bybit BTCUSDT`) carrying `order_client_id`.
fn unknown_report(order_client_id: Option<&str>) -> vike_model::OrderStatusReport {
    vike_model::OrderStatusReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        order_client_id: order_client_id.map(str::to_string),
        order_type: vike_model::OrderType::Stop,
        status: vike_model::OrderStatus::Accepted,
        side: -1,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        timestamp: 1,
        price: None,
        trigger_price: Some(90.0),
        trail: None,
        trail_percent: None,
    }
}

/// The stop leg of a v2 bracket: strategy `00A3`, account #7, group `k3x9q2m7d1p4`.
const V2_STOP_LEG: &str = "S00A307k3x9q2m7d1p4CLSL";

/// A v2 id's note names its group and role in words, after today's text, which it keeps whole: the
/// order client id readers of a held row (`vike_core`'s `held_row_order_client_id`, `symbol_before_order_client_id`) read the same.
#[test]
fn a_v2_unknown_order_names_its_group_and_role_in_words() {
    let d = ReconcileDivergence::UnknownOrder(unknown_report(Some(V2_STOP_LEG)));
    assert_eq!(
        d.describe(),
        "bybit BTCUSDT order_client_id=Some(\"S00A307k3x9q2m7d1p4CLSL\") status=\"ACCEPTED\" — strategy (auto) \
         · strategy 00A3 · account #7 · closes long (cannot open) · stop of group k3x9q2m7d1p4"
    );
    // The identity of the held row carries no words: the label never makes it a new row.
    assert_eq!(
        d.identity_detail(),
        "bybit BTCUSDT order_client_id=Some(\"S00A307k3x9q2m7d1p4CLSL\")"
    );
}

/// v1, `EXT-`, a venue's own hash, a foreign id and no id at all keep today's note BYTE FOR BYTE.
#[test]
fn every_id_that_is_not_v2_keeps_todays_note_byte_for_byte() {
    for (order_client_id, today) in [
        (
            Some("deadbeef17"),
            "bybit BTCUSDT order_client_id=Some(\"deadbeef17\") status=\"ACCEPTED\"",
        ),
        (
            Some("westVdeadbeef17"),
            "bybit BTCUSDT order_client_id=Some(\"westVdeadbeef17\") status=\"ACCEPTED\"",
        ),
        (
            Some("EXT-ORD-bybit-7"),
            "bybit BTCUSDT order_client_id=Some(\"EXT-ORD-bybit-7\") status=\"ACCEPTED\"",
        ),
        (
            Some("0xb395aaaa"),
            "bybit BTCUSDT order_client_id=Some(\"0xb395aaaa\") status=\"ACCEPTED\"",
        ),
        (Some("my hedge"), "bybit BTCUSDT order_client_id=Some(\"my hedge\") status=\"ACCEPTED\""),
        (None, "bybit BTCUSDT order_client_id=None status=\"ACCEPTED\""),
    ] {
        let d = ReconcileDivergence::UnknownOrder(unknown_report(order_client_id));
        assert_eq!(d.describe(), today, "{order_client_id:?}");
    }
}

/// Nothing relinks: a venue row of a sibling leg (same group, the stop) is an `UnknownOrder` of its
/// own while the entry leg rests locally, and `resolve` holds it on today's identity.
#[test]
fn a_sibling_leg_of_a_local_group_is_not_linked_to_it() {
    let entry = unknown_report(Some("S00A307k3x9q2m7d1p4OLEN"));
    let mut orders = indexmap::IndexMap::new();
    orders.insert(
        "S00A307k3x9q2m7d1p4OLEN".to_string(),
        OrderRecord::from_status_report(&entry).expect("an order client id"),
    );
    let local = ReconcileLocalView {
        venue: "bybit",
        orders: &orders,
        seen_trade_ids: &Default::default(),
        positions: &Default::default(),
        quantity_tolerance: 1e-9,
        cash: ReconcileLocalCash::default(),
    };
    let stop = unknown_report(Some(V2_STOP_LEG));
    let divergences = crate::recon::diff(&[entry, stop.clone()], &[], &[], &local, None);
    assert_eq!(
        divergences,
        vec![ReconcileDivergence::UnknownOrder(stop)],
        "the stop leg stands alone"
    );

    let held = ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() };
    let recon = crate::recon::resolve(divergences.clone(), &held, None, None);
    assert!(recon.events.is_empty(), "{:?}", recon.events);
    assert_eq!(recon.alerts.len(), 1, "{:?}", recon.alerts);
    let alert = &recon.alerts[0];
    assert_eq!(alert.detail, divergences[0].describe());
    assert_eq!(alert.identity_detail.as_deref(), Some(divergences[0].identity_detail().as_str()));
    assert_eq!(alert.dedup_key, None);
}

/// A held row from ANOTHER instance names its tag ONCE: a tagged v2 id's words say `instance bx2`,
/// so the foreign note keeps its warning and drops `origin=`; a v1 id's note keeps `origin=` as ever.
#[test]
fn a_foreign_v2_id_names_the_other_instance_once() {
    let held = ReconcilePolicy {
        default: ReconcileMode::Quarantine,
        local_instance_origin: Some(vike_model::OrderClientIdOrigin::parse("ap1").unwrap()),
        ..Default::default()
    };
    let detail_of = |order_client_id: &str| {
        let d = ReconcileDivergence::UnknownOrder(unknown_report(Some(order_client_id)));
        let recon = crate::recon::resolve(vec![d], &held, None, None);
        assert_eq!(recon.alerts.len(), 1, "{order_client_id}: {:?}", recon.alerts);
        recon.alerts[0].detail.clone()
    };
    const WARNING: &str = "two deployments are trading this venue account; nothing was folded \
                           into local books";

    let v2 = detail_of("bx2WS00A307k3x9q2m7d1p4CLSL");
    assert_eq!(
        v2,
        format!(
            "bybit BTCUSDT order_client_id=Some(\"bx2WS00A307k3x9q2m7d1p4CLSL\") status=\"ACCEPTED\" — \
             strategy (auto) · strategy 00A3 · account #7 · closes long (cannot open) · stop of \
             group k3x9q2m7d1p4 · instance bx2 — placed by ANOTHER INSTANCE (this instance is \
             ap1) — {WARNING}"
        )
    );

    let v1 = detail_of("bx2Vdeadbeef7");
    assert_eq!(
        v1,
        format!(
            "bybit BTCUSDT order_client_id=Some(\"bx2Vdeadbeef7\") status=\"ACCEPTED\" — placed by ANOTHER \
             INSTANCE (origin=bx2, this instance is ap1) — {WARNING}"
        ),
        "a v1 id's note is today's"
    );
    // An untagged v2 id names no instance in its words and is never foreign: no note at all.
    assert!(!detail_of("S00A307k3x9q2m7d1p4CLSL").contains("ANOTHER INSTANCE"));
}

/// ...and it must name the SYMBOL, which is the first thing an operator needs.
///
/// ⚠ Exempt by construction, not by oversight: `OrphanLocalOrder` carries an order client id and
/// no symbol, `BalanceDrift` is per ASSET rather than per instrument, and `JournalDivergence`
/// forwards a detail built elsewhere. Naming them here is what keeps the exemption a decision.
#[test]
fn every_instrument_shaped_divergence_names_its_symbol() {
    for d in every_variant() {
        let exempt = matches!(
            d.kind(),
            ReconcileDivergenceKind::OrphanLocalOrder
                | ReconcileDivergenceKind::BalanceDrift
                | ReconcileDivergenceKind::JournalDivergence
        );
        if exempt {
            continue;
        }
        assert!(
            d.describe().contains("BTCUSDT"),
            "{:?} does not name the symbol: {}",
            d.kind(),
            d.describe()
        );
    }
}
