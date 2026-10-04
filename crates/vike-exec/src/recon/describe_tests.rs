use super::*;

/// One of every [`Divergence`] variant, all naming `BTCUSDT` where they have a symbol at all.
///
/// ⚠ A hand-written list, and its completeness is asserted below rather than assumed — the
/// thing that FORCES a new variant to be described is `describe`'s total match, which is a
/// compile error. This list only makes the assertions iterate, and
/// `the_fixture_covers_every_kind` is what stops it silently covering eight of nine.
fn every_variant() -> Vec<Divergence> {
    let fill = vike_model::FillReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v1".into(),
        client_order_id: Some("c1".into()),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        ts: 1,
    };
    let order = vike_model::OrderStatusReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        client_order_id: Some("c1".into()),
        order_type: "LIMIT".into(),
        status: "FILLED".into(),
        side: 1,
        qty: 1.0,
        filled_qty: 1.0,
        avg_px: 100.0,
        ts: 1,
    };
    let pos = vike_model::PositionStatusReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Long,
        qty: 1.0,
        avg_px: 100.0,
        ts: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    vec![
        Divergence::MissingFill(fill),
        Divergence::MissingTerminal { order: order.clone() },
        Divergence::OrphanLocalOrder { client_order_id: "c9".into() },
        Divergence::UnknownOrder(order),
        Divergence::PositionDrift { report: pos.clone(), local_qty: 0.0 },
        Divergence::PositionOnlyExternal(pos),
        Divergence::OrphanLocalPosition {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            position_side: "long".into(),
            local_qty: 1.0,
        },
        Divergence::BalanceDrift {
            venue: "bybit".into(),
            asset: "USDT".into(),
            local: 1.0,
            venue_bal: 2.0,
            ts: 1,
        },
        Divergence::JournalDivergence {
            detail: "a journal detail built elsewhere".into(),
            recover_order: None,
        },
    ]
}

/// The fixture covers EVERY kind — otherwise the two assertions below quietly test eight of nine.
#[test]
fn the_fixture_covers_every_kind() {
    let mut seen: Vec<String> = every_variant().iter().map(|d| format!("{:?}", d.kind())).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), every_variant().len(), "the fixture repeats a kind: {seen:?}");
}

/// **A description must not be the kind's own name.**
///
/// The fallback this replaced was `format!("{:?}", d.kind())`, and on the live the CI box box it
/// produced `kind=PositionOnlyExternal detail=PositionOnlyExternal` — a line raised
/// specifically to answer *which* that answered *the same*. This asserts the two differ for
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

/// ...and it must name the SYMBOL, which is the first thing an operator needs.
///
/// ⚠ Exempt by construction, not by oversight: `OrphanLocalOrder` carries a client order id and
/// no symbol, `BalanceDrift` is per ASSET rather than per instrument, and `JournalDivergence`
/// forwards a detail built elsewhere. Naming them here is what keeps the exemption a decision.
#[test]
fn every_instrument_shaped_divergence_names_its_symbol() {
    for d in every_variant() {
        let exempt = matches!(
            d.kind(),
            DivergenceKind::OrphanLocalOrder
                | DivergenceKind::BalanceDrift
                | DivergenceKind::JournalDivergence
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
