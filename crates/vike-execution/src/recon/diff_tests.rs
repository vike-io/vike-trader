use super::*;
use indexmap::IndexMap;
use std::assert_matches;
use vike_model::OrderRecord;
use vike_model::OrderRequest;
use vike_model::OrderStatus;
use vike_model::events::{LiquiditySide, PositionSide};

fn empty_local<'a>(
    orders: &'a IndexMap<String, vike_model::OrderRecord>,
    seen: &'a HashSet<String>,
    pos: &'a IndexMap<(String, PositionSide), f64>,
) -> ReconcileLocalView<'a> {
    cash_local(orders, seen, pos, ReconcileLocalCash::default())
}

fn fill_report(trade_id: &'static str) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v1".into(),
        order_client_id: Some("c1".into()),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
    }
}

/// A venue fill no local record has seen is exactly one `MissingFill` (the two tests below
/// state it from the two-way and the journal-less sides).
fn assert_an_unseen_fill_is_one_missing_fill() {
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[fill_report("t-new")], &[], &local, None);
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::MissingFill(_));
}

#[test]
fn unseen_fill_is_missing_fill() {
    assert_an_unseen_fill_is_one_missing_fill();
}

#[test]
fn seen_fill_is_no_divergence() {
    let orders = IndexMap::new();
    let mut seen = HashSet::new();
    seen.insert("t-old".to_string());
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[fill_report("t-old")], &[], &local, None);
    assert!(d.is_empty());
}

#[test]
fn venue_only_position_is_external() {
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let p = PositionStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        qty: 2.0,
        average_price: 100.0,
        timestamp: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    let d = diff(&[], &[], &[p], &local, None);
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::PositionOnlyExternal(_));
}

#[test]
fn position_drift_beyond_tolerance() {
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let mut pos = IndexMap::new();
    pos.insert(("BTCUSDT".to_string(), PositionSide::Both), 1.0);
    let local = empty_local(&orders, &seen, &pos);
    let p = PositionStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        qty: 2.0,
        average_price: 100.0,
        timestamp: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    let d = diff(&[], &[], &[p], &local, None);
    assert_matches!(d[0], ReconcileDivergence::PositionDrift { local_quantity, .. } if local_quantity == 1.0);
}

// --- the local-side position sweep (step 5): OrphanLocalPosition ---

fn position_report(symbol: &str, side: PositionSide, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: side,
        qty,
        average_price: 100.0,
        timestamp: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

fn local_positions(rows: &[(&str, PositionSide, f64)]) -> IndexMap<(String, PositionSide), f64> {
    rows.iter().map(|(symbol, side, q)| ((symbol.to_string(), *side), *q)).collect()
}

fn orphan_positions(d: &[ReconcileDivergence]) -> Vec<&ReconcileDivergence> {
    d.iter().filter(|x| matches!(x, ReconcileDivergence::OrphanLocalPosition { .. })).collect()
}

#[test]
fn local_position_the_venue_never_reported_is_an_orphan() {
    // Local holds +1 BTCUSDT; the venue's report mentions only a (flat) ETHUSDT row. The
    // venue-anchored position leg alone raises nothing here.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[("BTCUSDT", PositionSide::Both, 1.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("ETHUSDT", PositionSide::Both, 0.0)], &local, None);
    assert_eq!(d.len(), 1, "{d:?}");
    match &d[0] {
        ReconcileDivergence::OrphanLocalPosition {
            venue,
            symbol,
            position_side,
            local_quantity,
        } => {
            assert_eq!(venue, "binance", "echoes the ReconcileLocalView's venue");
            assert_eq!(symbol, "BTCUSDT");
            assert_eq!(*position_side, PositionSide::Both);
            assert_eq!(*local_quantity, 1.0);
        }
        other => panic!("expected OrphanLocalPosition, got {other:?}"),
    }
}

#[test]
fn venue_reporting_the_position_flat_stays_position_drift_not_an_orphan() {
    // No double-reporting: a venue row that IS present — including an explicitly FLAT one, the
    // shape several ReconClients deliberately emit — keeps the disagreement on the existing
    // PositionDrift path. The sweep must not raise a second divergence for the same key.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[("BTCUSDT", PositionSide::Both, 1.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 0.0)], &local, None);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_matches!(d[0], ReconcileDivergence::PositionDrift { local_quantity, .. } if local_quantity == 1.0);
    assert!(orphan_positions(&d).is_empty(), "no orphan for a key the venue DID report");
}

#[test]
fn empty_venue_position_report_never_sweeps_local_positions() {
    // The gate. An EMPTY position report is indistinguishable from "this venue has no position
    // concept / the fetch is not implemented" — every orders-only ReconcileClient returns
    // Ok(Vec::new()) — so it must sweep NOTHING, or every such book would raise a permanent,
    // un-healable divergence on every pass.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[
        ("BTCUSDT", PositionSide::Both, 1.0),
        ("ETHUSDT", PositionSide::Both, -3.0),
    ]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[], &local, None);
    assert!(d.is_empty(), "empty position report sweeps nothing: {d:?}");
}

#[test]
fn flat_local_position_rows_are_not_orphans() {
    // `Account::positions` keeps a key after it closes, so the local book routinely holds
    // zero/near-zero rows. Only genuinely-open risk (beyond quantity_tolerance) may raise.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[
        ("BTCUSDT", PositionSide::Both, 0.0),
        ("SOLUSDT", PositionSide::Both, 1e-12),
    ]);
    let local = empty_local(&orders, &seen, &pos); // quantity_tolerance = 1e-9
    let d = diff(&[], &[], &[position_report("ETHUSDT", PositionSide::Both, 0.0)], &local, None);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn orphan_local_position_is_keyed_by_side_not_symbol_alone() {
    // Hedge mode: the venue reports the LONG leg only. The SHORT leg is a separate key with no
    // venue row of its own, so it is the orphan — matching a symbol alone would miss it.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[
        ("BTCUSDT", PositionSide::Long, 1.0),
        ("BTCUSDT", PositionSide::Short, -2.0),
    ]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Long, 1.0)], &local, None);
    let orphans = orphan_positions(&d);
    assert_eq!(d.len(), 1, "the LONG leg agrees, so the SHORT orphan is the ONLY finding: {d:?}");
    assert_eq!(orphans.len(), 1, "{d:?}");
    assert_matches!(
        orphans[0],
        ReconcileDivergence::OrphanLocalPosition { position_side, local_quantity, .. }
            if *position_side == PositionSide::Short && *local_quantity == -2.0
    );
}

/// **A hand trade on a spot pair is the user's, not a missing fill** (the spot rule's answer 2): a venue fill with
/// no order client id on a symbol this pass reports as a `Cash` holding is a trade placed by hand in the venue's
/// app, which the fold keeps out of the strategy's position, so reconcile raises no `MissingFill` that `hybrid`
/// would fold or a confirm would book. A fill naming an order, and an order-client-id-less fill on a derivative, still are.
#[test]
fn a_hand_fill_on_a_spot_pair_is_no_missing_fill() {
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let wallet = PositionStatusReport::spot_holding("binance", "BTCUSDT", 1.0, 0.0001, 1);
    let hand = FillReport { order_client_id: None, ..fill_report("t-hand") };
    let blank = FillReport { order_client_id: Some(String::new()), ..fill_report("t-blank") };
    let ours = fill_report("t-ours");
    let d = diff(&[], &[hand.clone(), blank, ours], &[wallet], &local, None);
    let missing: Vec<&str> = d
        .iter()
        .filter_map(|x| match x {
            ReconcileDivergence::MissingFill(f) => Some(f.trade_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(missing, ["t-ours"], "only the fill of an order is the strategy's: {d:?}");
    // the same order-client-id-less fill with no spot row for its symbol (a derivative's book) is still missing
    let d =
        diff(&[], &[hand], &[position_report("BTCUSDT", PositionSide::Both, 0.0)], &local, None);
    assert_matches!(&d[..], [ReconcileDivergence::MissingFill(_)]);
}

/// **A spot (`Cash`) row is a FLOOR** (the spot rule, owner 2026-11-11): the wallet holding the strategy's coins
/// AND MORE agrees (the surplus is the user's), a coin the engine never bought raises nothing, and only a wallet
/// holding LESS than the engine's position is a divergence (coins sold or withdrawn by hand).
#[test]
fn a_spot_row_is_a_floor_the_users_surplus_raises_nothing() {
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[("DOGEUSDT.S", PositionSide::Both, 5.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let spot =
        |symbol: &str, held| PositionStatusReport::spot_holding("bybit", symbol, held, 0.1, 1);
    // the wallet holds the strategy's 5 and the user's 95: agreement
    let d = diff(&[], &[], &[spot("DOGEUSDT.S", 100.0)], &local, None);
    assert!(d.is_empty(), "the surplus is the user's: {d:?}");
    // the user's own coins on a pair the engine holds nothing of: no PositionOnlyExternal
    let d = diff(&[], &[], &[spot("DOGEUSDT.S", 5.0), spot("XRPUSDT.S", 40.0)], &local, None);
    assert!(d.is_empty(), "a coin the engine never bought is the user's: {d:?}");
    // the wallet holds LESS than the strategy bought: the one divergence, against the local figure
    let d = diff(&[], &[], &[spot("DOGEUSDT.S", 3.0)], &local, None);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_matches!(&d[0], ReconcileDivergence::PositionDrift { report, local_quantity } if report.qty == 3.0 && *local_quantity == 5.0);
    // ...the wallet empty too (a flat row: dust under one step)
    let d = diff(&[], &[], &[spot("DOGEUSDT.S", 0.05)], &local, None);
    assert_matches!(&d[..], [ReconcileDivergence::PositionDrift { report, .. }] if report.qty == 0.0);
    // a derivative row keeps the two-sided rule: a venue row larger than the book is still a drift
    let d =
        diff(&[], &[], &[position_report("DOGEUSDT.S", PositionSide::Both, 100.0)], &local, None);
    assert_matches!(&d[..], [ReconcileDivergence::PositionDrift { .. }]);
}

/// The the CI box case (2026-11-12): bought 0.007 ETH less a 0.000007 ETH fee, sold 0.0069, so the book holds 0.000093
/// that the 0.0001 step can never sell. The wallet row is flat under one step (`spot_holding`), and so is the local
/// figure: no drift. With no step known the local figure is compared unrounded, as before.
#[test]
fn spot_dust_under_one_step_is_flat_on_both_sides() {
    let (orders, seen) = (IndexMap::new(), HashSet::new());
    // 0.006993 - 0.0069 in f64, the book's figure on the CI box
    let pos = local_positions(&[("ETHUSDT", PositionSide::Both, 0.0000930000000000002)]);
    let local = empty_local(&orders, &seen, &pos);
    let wallet = [PositionStatusReport::spot_holding("binance", "ETHUSDT", 0.000093, 0.0001, 1)];
    assert_eq!(wallet[0].qty, 0.0);
    let d = diff_scoped(&[], &[], &wallet, &local, None, &[], &|_| Some(0.0001));
    assert!(d.is_empty(), "dust under one step is flat on both sides: {d:?}");
    let d = diff_scoped(&[], &[], &wallet, &local, None, &[], &|_| None);
    assert_matches!(
        &d[..],
        [ReconcileDivergence::PositionDrift { .. }],
        "no step known: unrounded"
    );
}

/// Rounding only flattens a local figure UNDER one step: a book a step or more above the wallet still drifts.
#[test]
fn a_spot_drift_above_one_step_still_fires() {
    let (orders, seen) = (IndexMap::new(), HashSet::new());
    let pos = local_positions(&[("ETHUSDT", PositionSide::Both, 0.0102)]);
    let local = empty_local(&orders, &seen, &pos);
    let lot = |_: &str| Some(0.0001);
    for held in [0.0100, 0.0] {
        let wallet = [PositionStatusReport::spot_holding("binance", "ETHUSDT", held, 0.0001, 1)];
        let d = diff_scoped(&[], &[], &wallet, &local, None, &[], &lot);
        assert_matches!(
            &d[..],
            [ReconcileDivergence::PositionDrift { report, local_quantity }] if report.qty == held && *local_quantity == 0.0102,
            "wallet {held}"
        );
    }
}

/// A derivative row is untouched by the step: a sub-step local figure against a flat venue row drifts exactly as
/// `diff` (no step) says.
#[test]
fn a_derivative_sub_step_difference_is_unchanged() {
    let (orders, seen) = (IndexMap::new(), HashSet::new());
    let pos = local_positions(&[("ETHUSDT", PositionSide::Both, 0.000093)]);
    let local = empty_local(&orders, &seen, &pos);
    let venue = [position_report("ETHUSDT", PositionSide::Both, 0.0)];
    let d = diff_scoped(&[], &[], &venue, &local, None, &[], &|_| Some(0.0001));
    assert_matches!(&d[..], [ReconcileDivergence::PositionDrift { .. }]);
    assert_eq!(d, diff(&[], &[], &venue, &local, None));
}

/// A symbol with no row of its own borrows no step from the MOUNT (`RiskLimits::own_lot_size`, the core's step
/// source): an unadmitted spot pair's local figure under the mount's lot still drifts, where `grid_for`'s mount
/// fallback would flatten it. The mounted symbol rounds on its lot, and an admitted pair on its own row's.
#[test]
fn an_unadmitted_spot_symbol_takes_no_lot_from_the_mount() {
    let (orders, seen) = (IndexMap::new(), HashSet::new());
    let pos = local_positions(&[
        ("BTCUSDT", PositionSide::Both, 0.0005),
        ("ETHUSDT", PositionSide::Both, 0.0005),
    ]);
    let local = empty_local(&orders, &seen, &pos);
    let wallet = [
        PositionStatusReport::spot_holding("binance", "BTCUSDT", 0.0, 0.001, 1),
        PositionStatusReport::spot_holding("binance", "ETHUSDT", 0.0, 0.001, 1),
    ];
    let mut limits = vike_model::RiskLimits { lot_size: Some(0.001), ..Default::default() };
    assert_eq!(limits.grid_for("ETHUSDT").lot_size, Some(0.001), "grid_for lends the mount's lot");

    let d =
        diff_scoped(&[], &[], &wallet, &local, None, &[], &|s| limits.own_lot_size(s, "BTCUSDT"));
    assert_matches!(
        &d[..],
        [ReconcileDivergence::PositionDrift { report, .. }] if report.symbol == "ETHUSDT",
        "only the unadmitted pair drifts"
    );

    let admitted = vike_model::SymbolGrid {
        tick_size: Some(0.01),
        lot_size: Some(0.001),
        on_demand: true,
        ..Default::default()
    };
    limits.grid_by_symbol.insert("ETHUSDT".into(), admitted);
    let d =
        diff_scoped(&[], &[], &wallet, &local, None, &[], &|s| limits.own_lot_size(s, "BTCUSDT"));
    assert!(d.is_empty(), "an admitted pair rounds on its own lot: {d:?}");
}

/// Pins THE DRIFT COMPARISON ITSELF — `(local_quantity - p.qty).abs() > quantity_tolerance` — against its `-`
/// becoming `+` or `/`: every other position test either agrees on both sides or has no local key
/// at all, and both shapes are blind to the operator between the two numbers.
///
/// This matters because `PositionDrift` is the ONE no-local-origin kind that `hybrid`
/// auto-applies: it rewrites local position size onto the venue's number and books realized PnL
/// at the venue's average price with no operator in front of it. A comparison that computes the
/// wrong delta therefore either fires when the books agree, or stays silent while they do not.
///
/// Three shapes, chosen so that each mutation is caught by at least one:
#[test]
fn the_drift_test_is_a_difference_not_a_sum_or_a_ratio() {
    let orders = IndexMap::new();
    let seen = HashSet::new();

    // 1. SIGN-OPPOSED: local is short 1, the venue says long 1. The true delta is 2 (a
    //    divergence); a SUM is 0 (silence). This is also the only production feeder of
    //    `synth_position_legs`' zero-crossing PnL branch, so silence here is expensive.
    let pos = local_positions(&[("BTCUSDT", PositionSide::Both, -1.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 1.0)], &local, None);
    assert_eq!(d.len(), 1, "sign-opposed books are a drift: {d:?}");
    assert_matches!(
        &d[0], ReconcileDivergence::PositionDrift { local_quantity, .. } if *local_quantity == -1.0,
        "{d:?}"
    );

    // 2. FLAT LOCAL vs a live venue position. The true delta is 5; a RATIO is 0/5 = 0
    //    (silence). A 0.0 local key is the ordinary shape, not a contrived one —
    //    `Account::apply_fill` REBINDS a closed key to the new size rather than removing it,
    //    so a flat-but-known symbol is exactly what `local_view` copies.
    let pos = local_positions(&[("BTCUSDT", PositionSide::Both, 0.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 5.0)], &local, None);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_matches!(
        &d[0], ReconcileDivergence::PositionDrift { local_quantity, .. } if *local_quantity == 0.0,
        "a KNOWN-but-flat key drifts; it is not PositionOnlyExternal, which is for keys \
             local has never heard of: {d:?}"
    );

    // 3. THE AGREEING CONTROL. Without it a mutation that fires ALWAYS would still pass the
    //    two above — they only prove the comparison is not silent.
    let pos = local_positions(&[("BTCUSDT", PositionSide::Both, 3.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 3.0)], &local, None);
    assert!(d.is_empty(), "books that agree are not a divergence: {d:?}");
}

#[test]
fn venue_only_positions_with_an_empty_local_book_are_unchanged() {
    // The pre-existing venue-anchored behavior is untouched when local holds nothing.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let reports = [
        position_report("BTCUSDT", PositionSide::Both, 2.0),
        position_report("ETHUSDT", PositionSide::Both, -1.0),
    ];
    let d = diff(&[], &[], &reports, &local, None);
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d.iter().all(|x| matches!(x, ReconcileDivergence::PositionOnlyExternal(_))));
}

#[test]
fn orphan_local_positions_are_emitted_after_the_venue_position_leg() {
    // Deterministic ordering (the module doc's contract): the venue-anchored leg first, then
    // the local-side sweep — the same shape the order legs already have.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = local_positions(&[("BTCUSDT", PositionSide::Both, 1.0)]);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[], &[], &[position_report("ETHUSDT", PositionSide::Both, 2.0)], &local, None);
    assert_eq!(d.len(), 2, "{d:?}");
    assert_matches!(d[0], ReconcileDivergence::PositionOnlyExternal(_));
    assert_matches!(d[1], ReconcileDivergence::OrphanLocalPosition { .. });
}

// --- three-way (journal) cross-check ---

#[test]
fn journal_none_matches_two_way_missing_fill() {
    assert_an_unseen_fill_is_one_missing_fill();
}

#[test]
fn trade_in_neither_local_nor_journal_stays_missing_fill() {
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let journal = ReconcileJournalView { seen_trade_ids: HashSet::new(), orders: IndexMap::new() };
    let d = diff(&[], &[fill_report("t-new")], &[], &local, Some(&journal));
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::MissingFill(_));
}

#[test]
fn trade_in_journal_but_not_local_is_journal_divergence() {
    let orders = IndexMap::new();
    let seen = HashSet::new(); // local lost it
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let mut journal_seen = HashSet::new();
    journal_seen.insert("t-restored".to_string());
    let journal = ReconcileJournalView { seen_trade_ids: journal_seen, orders: IndexMap::new() };
    let d = diff(&[], &[fill_report("t-restored")], &[], &local, Some(&journal));
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::JournalDivergence { .. });
}

#[test]
fn trade_in_both_local_and_journal_is_no_divergence() {
    let orders = IndexMap::new();
    let mut seen = HashSet::new();
    seen.insert("t-old".to_string());
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let mut journal_seen = HashSet::new();
    journal_seen.insert("t-old".to_string());
    let journal = ReconcileJournalView { seen_trade_ids: journal_seen, orders: IndexMap::new() };
    let d = diff(&[], &[fill_report("t-old")], &[], &local, Some(&journal));
    assert!(d.is_empty());
}

fn order_report(order_client_id: &str, status: OrderStatus) -> OrderStatusReport {
    OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some(order_client_id.into()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status,
        timestamp: 1,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

fn journal_with_order(order_client_id: &str, status: OrderStatus) -> ReconcileJournalView {
    let mut orders = IndexMap::new();
    orders.insert(order_client_id.to_string(), status);
    ReconcileJournalView { seen_trade_ids: HashSet::new(), orders }
}

#[test]
fn venue_order_not_in_local_no_journal_is_unknown_order() {
    // journal: None → the two-way behavior, UnknownOrder.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[order_report("c-ext", OrderStatus::Accepted)], &[], &[], &local, None);
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::UnknownOrder(_));
}

#[test]
fn venue_order_live_in_journal_but_not_local_is_journal_divergence() {
    // the journal recorded this order as still LIVE (ACCEPTED) and the venue reports it, but
    // local in-memory state lost it → a restore/persistence bug, not a plain unknown order.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let journal = journal_with_order("c-lost", OrderStatus::Accepted);
    let d =
        diff(&[order_report("c-lost", OrderStatus::Accepted)], &[], &[], &local, Some(&journal));
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::JournalDivergence { .. });
}

#[test]
fn venue_order_terminal_in_journal_but_not_local_stays_unknown_order() {
    // the journal only has the order as already-terminal (FILLED) — local correctly reaped it,
    // so its absence is NOT a persistence bug: it stays a plain UnknownOrder, not a divergence.
    let orders = IndexMap::new();
    let seen = HashSet::new();
    let pos = IndexMap::new();
    let local = empty_local(&orders, &seen, &pos);
    let journal = journal_with_order("c-done", OrderStatus::Filled);
    let d = diff(&[order_report("c-done", OrderStatus::Filled)], &[], &[], &local, Some(&journal));
    assert_eq!(d.len(), 1);
    assert_matches!(d[0], ReconcileDivergence::UnknownOrder(_));
}

// --- diff_balance (first-class cash reconcile) ---

use super::super::types::{ReconcileBalanceTolerance, ReconcileLocalCash};
use crate::account::BalanceMode;

/// A `ReconcileLocalView` carrying a specific cash slice (orders/positions empty — irrelevant here).
fn cash_local<'a>(
    orders: &'a IndexMap<String, vike_model::OrderRecord>,
    seen: &'a HashSet<String>,
    pos: &'a IndexMap<(String, PositionSide), f64>,
    cash: ReconcileLocalCash,
) -> ReconcileLocalView<'a> {
    ReconcileLocalView {
        venue: "binance",
        orders,
        seen_trade_ids: seen,
        positions: pos,
        quantity_tolerance: 1e-9,
        cash,
    }
}

fn authoritative(balance: f64, realized_pnl: f64, realized_at_sync: f64) -> ReconcileLocalCash {
    ReconcileLocalCash {
        balance,
        realized_pnl,
        realized_at_sync: Some(realized_at_sync),
        mode: BalanceMode::Authoritative,
    }
}

/// `true` when the verdict raised no drift — which is NOT the same as "adopt", and telling the
/// two apart is the whole reason [`ReconcileBalanceCheck`] exists. Every call site below states which
/// of the three verdicts it expects; this helper only spares the ones whose subject is the
/// drift alone.
fn no_drift(c: &ReconcileBalanceCheck) -> bool {
    !matches!(c, ReconcileBalanceCheck::Anchored { drift: Some(_), .. })
}

fn drift(c: &ReconcileBalanceCheck) -> Option<&ReconcileDivergence> {
    match c {
        ReconcileBalanceCheck::Anchored { drift, .. } => drift.as_ref(),
        _ => None,
    }
}

/// [`diff_balance`] of `cash` against the venue's `venue` figure (USDT, the default tolerance,
/// `ts` 1) over a view whose orders and positions are empty.
fn balance_check(cash: ReconcileLocalCash, venue: Option<f64>) -> ReconcileBalanceCheck {
    let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
    diff_balance(
        &cash_local(&o, &s, &p, cash),
        venue,
        "USDT",
        ReconcileBalanceTolerance::default(),
        1,
    )
}

#[test]
fn diff_balance_not_reported_when_venue_balance_absent() {
    // ⚠ NOT `Anchored`: a venue that reported nothing is evidence of neither agreement nor
    // disagreement, and the caller must write nothing at all — not even the roll-forward.
    assert_eq!(
        balance_check(authoritative(10_000.0, 0.0, 0.0), None),
        ReconcileBalanceCheck::NotReported
    );
}

#[test]
fn diff_balance_adopts_in_delta_first_observation() {
    // Delta mode: `balance` is the arbitrary seed_cash, not venue truth — never flag even when
    // the raw numbers differ wildly. The caller ADOPTS the venue's figure whole.
    let delta = ReconcileLocalCash { balance: 10_000.0, ..ReconcileLocalCash::default() };
    assert_eq!(balance_check(delta, Some(50_000.0)), ReconcileBalanceCheck::Adopt(50_000.0));
}

#[test]
fn diff_balance_adopts_without_baseline() {
    // Authoritative but never synced (realized_at_sync None): still first-observation → adopt.
    let no_baseline = ReconcileLocalCash {
        balance: 10_000.0,
        realized_pnl: 500.0,
        realized_at_sync: None,
        mode: BalanceMode::Authoritative,
    };
    assert_eq!(balance_check(no_baseline, Some(12_000.0)), ReconcileBalanceCheck::Adopt(12_000.0));
}

#[test]
fn diff_balance_within_tolerance_is_anchored_and_never_adopt() {
    // venue == expected within the 1-unit floor: no divergence — and, the load-bearing half,
    // NOT an adopt. `expected` is the LOCAL figure rolled forward, so the venue's 10_000.5 is
    // nowhere in the answer: a caller applying this verdict cannot absorb the 0.5.
    // expected = 10_000; venue 10_000.5 is inside max(1.0, 1e-4·10_000.5 = 1.00005) = 1.00005.
    assert_eq!(
        balance_check(authoritative(10_000.0, 0.0, 0.0), Some(10_000.5)),
        ReconcileBalanceCheck::Anchored { expected: 10_000.0, drift: None }
    );
}

/// **The property the every-pass re-adopt destroyed**: applying an `Anchored` verdict leaves
/// the NEXT pass's `expected` bit-unchanged, so a repeated sub-threshold move accumulates
/// against a FIXED reference instead of being absorbed. Both splits of the same total are
/// tried — the pre-roll one (`balance` behind, `realized_at_sync` old) and the post-roll one
/// (`balance` rolled forward, baseline re-anchored to `realized_pnl`).
///
/// Written as an ARITHMETIC identity rather than a runtime walk on purpose: the runtime pin
/// (`crates/vike-tradehub-core/tests/recon/reconcile_balance.rs`'s
/// `a_sub_tolerance_creep_is_not_absorbed_pass_after_pass`) drives the passes, and this one
/// says why the write it performs is safe.
#[test]
fn rolling_the_local_figure_forward_leaves_expected_invariant() {
    let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
    // Anchor: balance 10_000 with realized 200 folded in; the account has since realized +800.
    let before = cash_local(&o, &s, &p, authoritative(10_000.0, 1000.0, 200.0));
    let ReconcileBalanceCheck::Anchored { expected, .. } =
        diff_balance(&before, Some(10_800.0), "USDT", ReconcileBalanceTolerance::default(), 1)
    else {
        panic!("a synced account diffs");
    };
    assert_eq!(expected, 10_800.0);
    // Now the state AFTER the caller applies it: balance := expected, baseline := realized_pnl.
    let after = cash_local(&o, &s, &p, authoritative(expected, 1000.0, 1000.0));
    let ReconcileBalanceCheck::Anchored { expected: again, drift: still_agrees } =
        diff_balance(&after, Some(10_800.0), "USDT", ReconcileBalanceTolerance::default(), 1)
    else {
        panic!("still a synced account");
    };
    assert_eq!(again.to_bits(), expected.to_bits(), "the roll-forward is a no-op for the diff");
    assert!(still_agrees.is_none());
    // ...and the accumulation that buys: the venue creeps 0.6 per pass, each step inside the
    // 1.00006 band, and by the third the FIXED anchor has seen 1.8 — a drift a re-anchoring
    // pass would have absorbed three times over.
    let creep =
        diff_balance(&after, Some(10_801.8), "USDT", ReconcileBalanceTolerance::default(), 1);
    assert!(drift(&creep).is_some(), "cumulative creep must cross the band: {creep:?}");
}

#[test]
fn diff_balance_realized_pnl_since_seed_does_not_trip() {
    // THE CONFOUND GUARD. Last sync captured realized_at_sync = 200. Since then the account
    // realized +800 more (realized_pnl now 1000) but `balance` did NOT move (Authoritative mode
    // does not fold realized into balance). The venue cash, however, DID grow by that +800 to
    // 10_800. expected = balance 10_000 + (1000 − 200) = 10_800 == venue → NO drift, despite an
    // 800-unit raw gap. A naive |balance − venue| diff would false-flag 800 here.
    assert!(no_drift(&balance_check(authoritative(10_000.0, 1000.0, 200.0), Some(10_800.0))));
}

#[test]
fn diff_balance_flags_genuine_surprise_beyond_tolerance() {
    // Same realized state as above (expected 10_800), but the venue reports 10_950 — an extra
    // +150 that realized PnL does NOT explain (a deposit / mis-track / haircut). Flag it.
    let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
    let local = cash_local(&o, &s, &p, authoritative(10_000.0, 1000.0, 200.0));
    let c = diff_balance(&local, Some(10_950.0), "USDT", ReconcileBalanceTolerance::default(), 7);
    // ⚠ A DRIFT pass still carries `expected`: the caller rolls the local figure forward on a
    // flagged pass too, or equity would freeze at the anchor while the account keeps trading.
    assert_matches!(
        &c, ReconcileBalanceCheck::Anchored { expected, .. } if *expected == 10_800.0,
        "a drift verdict still carries the rolled-forward local figure: {c:?}"
    );
    let d = drift(&c).expect("a 150-unit unexplained move must flag").clone();
    match d {
        ReconcileDivergence::BalanceDrift { venue, asset, local, venue_balance, timestamp: ts } => {
            assert_eq!(venue, "binance");
            assert_eq!(asset, "USDT");
            assert_eq!(local, 10_800.0, "carries the realized-corrected expected, not raw");
            assert_eq!(venue_balance, 10_950.0);
            assert_eq!(ts, 7);
        }
        other => panic!("expected BalanceDrift, got {other:?}"),
    }
}

#[test]
fn diff_balance_relative_band_scales_with_wallet_size() {
    // A 5-unit drift on a $10M wallet is inside the 1-bp relative band (1e-4·10M = 1000) → no
    // flag; the same 5-unit drift on a $1k wallet clears max(1.0, 0.1) = 1.0 → flags.
    let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
    let big = cash_local(&o, &s, &p, authoritative(10_000_000.0, 0.0, 0.0));
    assert!(no_drift(&diff_balance(
        &big,
        Some(10_000_005.0),
        "USDT",
        ReconcileBalanceTolerance::default(),
        1
    )));
    let small = cash_local(&o, &s, &p, authoritative(1_000.0, 0.0, 0.0));
    assert!(
        drift(&diff_balance(
            &small,
            Some(1_005.0),
            "USDT",
            ReconcileBalanceTolerance::default(),
            1
        ))
        .is_some()
    );
}

#[test]
fn diff_balance_honors_a_custom_tolerance() {
    // A NON-default ReconcileBalanceTolerance (the env-tunable knob path) actually moves the flag boundary.
    let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
    let local = cash_local(&o, &s, &p, authoritative(10_000.0, 0.0, 0.0)); // expected = 10_000

    // A 0.5 drift is WITHIN the default band (no flag) but TRIPS a tighter abs floor.
    assert!(no_drift(&diff_balance(
        &local,
        Some(10_000.5),
        "USDT",
        ReconcileBalanceTolerance::default(),
        1
    )));
    let tight = ReconcileBalanceTolerance { abs_floor: 0.1, rel_frac: 0.0 };
    assert!(drift(&diff_balance(&local, Some(10_000.5), "USDT", tight, 1)).is_some());

    // A 50.0 drift TRIPS the default band (flag) but is SUPPRESSED by a looser abs floor.
    assert!(
        drift(&diff_balance(
            &local,
            Some(10_050.0),
            "USDT",
            ReconcileBalanceTolerance::default(),
            1
        ))
        .is_some()
    );
    let loose = ReconcileBalanceTolerance { abs_floor: 100.0, rel_frac: 0.0 };
    assert!(no_drift(&diff_balance(&local, Some(10_050.0), "USDT", loose, 1)));
}

/// A local order whose order client id the venue ALSO reports on — the only shape that reaches
/// `o.status.is_terminal() && !mo.status.is_terminal()`, the whole `MissingTerminal` decision (every
/// other order test has an empty `local.orders` or a venue slice that never names the local order client id).
///
/// The three rows below are the truth table of that condition. Without all three, one mutation
/// or the other slips through: `&&` → `||` needs a case where the venue is live, and dropping
/// `!` needs a case where BOTH sides are terminal.
#[test]
fn missing_terminal_needs_the_venue_terminal_and_the_local_order_still_live() {
    // `OrderRecord`'s fields are `pub` and this is the same crate, so the baseline status is
    // ASSIGNED rather than driven through `apply` — the FSM would refuse most of these
    // transitions and the test would then prove nothing about `diff`.
    fn local_order(order_client_id: &str, status: OrderStatus) -> IndexMap<String, OrderRecord> {
        let mut mo = OrderRecord::new(OrderRequest {
            order_client_id: order_client_id.to_string(),
            ..Default::default()
        });
        mo.status = status;
        IndexMap::from([(order_client_id.to_string(), mo)])
    }
    let (seen, pos) = (HashSet::new(), IndexMap::new());

    // 1. Both sides say the order is LIVE — nothing to reconcile.
    let orders = local_order("c-1", OrderStatus::Accepted);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[order_report("c-1", OrderStatus::Accepted)], &[], &[], &local, None);
    assert!(d.is_empty(), "both sides live is not a divergence: {d:?}");

    // 2. The venue finished it, local still thinks it rests — THE case this branch exists for.
    let orders = local_order("c-1", OrderStatus::Accepted);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[order_report("c-1", OrderStatus::Filled)], &[], &[], &local, None);
    assert_eq!(d.len(), 1, "{d:?}");
    match &d[0] {
        ReconcileDivergence::MissingTerminal { order } => {
            // `as_deref`: `order_client_id` is `Option<String>` and there is no `PartialEq`
            // against `Option<&str>`.
            assert_eq!(order.order_client_id.as_deref(), Some("c-1"));
            assert_eq!(order.status, OrderStatus::Filled);
        }
        other => panic!("expected MissingTerminal, got {other:?}"),
    }

    // 3. Both terminal — local already knows. Deleting the `!` makes THIS the divergence.
    let orders = local_order("c-1", OrderStatus::Filled);
    let local = empty_local(&orders, &seen, &pos);
    let d = diff(&[order_report("c-1", OrderStatus::Filled)], &[], &[], &local, None);
    assert!(d.is_empty(), "an order local already knows is terminal is not a divergence: {d:?}");
}

// --- the scope of the two local-side sweeps: the symbols the pass's client READ (decision 0216) ---

fn live_local_order(order_client_id: &str, symbol: &str) -> IndexMap<String, OrderRecord> {
    let mut mo = OrderRecord::new(OrderRequest {
        order_client_id: order_client_id.to_string(),
        symbol: symbol.to_string(),
        ..Default::default()
    });
    mo.status = OrderStatus::Accepted;
    IndexMap::from([(order_client_id.to_string(), mo)])
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(ToString::to_string).collect()
}

/// A local order on a symbol the client did NOT read is not swept: the venue's report cannot
/// mention what nobody fetched. The same order on a symbol it read is, and `diff` (no scope) is the
/// scoped form with nothing unread.
#[test]
fn a_local_order_on_an_unread_symbol_is_not_an_orphan() {
    let (seen, pos) = (HashSet::new(), IndexMap::new());
    let orders = live_local_order("c-eth", "ETHUSDT");
    let local = empty_local(&orders, &seen, &pos);

    let read = diff_scoped(&[], &[], &[], &local, None, &[], &|_| None);
    assert_matches!(
        &read[..],
        [ReconcileDivergence::OrphanLocalOrder { order_client_id }] if order_client_id == "c-eth"
    );
    assert_eq!(
        diff(&[], &[], &[], &local, None),
        read,
        "diff is the scoped form with nothing unread"
    );

    let unread = diff_scoped(&[], &[], &[], &local, None, &strings(&["ETHUSDT"]), &|_| None);
    assert!(unread.is_empty(), "the client never fetched ETHUSDT: {unread:?}");

    let other = diff_scoped(&[], &[], &[], &local, None, &strings(&["SOLUSDT"]), &|_| None);
    assert_eq!(other.len(), 1, "another symbol's gap does not hide this one: {other:?}");
}

/// The position sweep keeps the same scope: an unread symbol's local position is left alone, a
/// read symbol's is swept, and an unread symbol a venue DID report is still judged (the scope
/// narrows the two local-side sweeps only, never the venue-anchored legs).
#[test]
fn a_local_position_on_an_unread_symbol_is_not_an_orphan() {
    let (orders, seen) = (IndexMap::new(), HashSet::new());
    let pos = local_positions(&[
        ("BTCUSDT", PositionSide::Both, 1.0),
        ("ETHUSDT", PositionSide::Both, -3.0),
    ]);
    let local = empty_local(&orders, &seen, &pos);
    let venue = [position_report("SOLUSDT", PositionSide::Both, 0.0)];

    let all = diff_scoped(&[], &[], &venue, &local, None, &[], &|_| None);
    assert_eq!(orphan_positions(&all).len(), 2, "{all:?}");

    let scoped = diff_scoped(&[], &[], &venue, &local, None, &strings(&["ETHUSDT"]), &|_| None);
    assert_matches!(
        &scoped[..],
        [ReconcileDivergence::OrphanLocalPosition { symbol, .. }] if symbol == "BTCUSDT"
    );

    let reported = [position_report("ETHUSDT", PositionSide::Both, 5.0)];
    let drifted = diff_scoped(&[], &[], &reported, &local, None, &strings(&["ETHUSDT"]), &|_| None);
    assert!(
        drifted.iter().any(|d| matches!(d, ReconcileDivergence::PositionDrift { .. })),
        "a position the venue reports is judged whether or not the symbol is listed unread: {drifted:?}"
    );
}
