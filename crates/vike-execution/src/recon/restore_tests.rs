use super::*;
use vike_model::events::LiquiditySide;

fn scope(symbols: &[&str]) -> RestoreScope {
    RestoreScope {
        venue: "binance".into(),
        account: None,
        symbols: symbols.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// A full ref: venue + symbol + a plain limit kind + no account, the shape a current ownership
/// record has.
fn rf(order_client_id: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        order_client_id: order_client_id.into(),
        venue: Some("binance".into()),
        symbol: Some(symbol.into()),
        account: None,
        order_type: Some(vike_model::OrderType::Limit),
        ..Default::default()
    }
}

/// [`rf`] of a line written before the kind was recorded.
fn rf_unrecorded(order_client_id: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef { order_type: None, ..rf(order_client_id, symbol) }
}

/// [`rf`] recording that the order was of `kind`.
fn rf_of(order_client_id: &str, symbol: &str, kind: vike_model::OrderType) -> RestoredOrderRef {
    RestoredOrderRef { order_type: Some(kind), ..rf(order_client_id, symbol) }
}

/// `r` after the caller counted [`RESTORE_QUIET_PASSES`] quiet passes for it: silence now proves it
/// gone (for a kind the client can speak for).
fn quiet(r: RestoredOrderRef) -> RestoredOrderRef {
    RestoredOrderRef { quiet_passes: RESTORE_QUIET_PASSES, ..r }
}

/// [`rf`] that remembers the venue acknowledged it as `venue_order_id`.
fn rf_acked(order_client_id: &str, symbol: &str, venue_order_id: &str) -> RestoredOrderRef {
    RestoredOrderRef { venue_order_id: Some(venue_order_id.into()), ..rf(order_client_id, symbol) }
}

/// [`plan_restore`] of the one restored ref `c1` on BTCUSDT, over a scope covering BTCUSDT.
fn plan_c1(reports: &[OrderStatusReport], fills: &[FillReport]) -> RestorePlan {
    plan_restore(&scope(&["BTCUSDT"]), &[rf("c1", "BTCUSDT")], reports, fills)
}

fn unproven_order_client_ids(p: &RestorePlan) -> Vec<&str> {
    p.unproven.iter().map(|r| r.order_client_id.as_str()).collect()
}

fn report(order_client_id: Option<&str>, symbol: &str, status: OrderStatus) -> OrderStatusReport {
    OrderStatusReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        venue_order_id: "v1".into(),
        order_client_id: order_client_id.map(str::to_string),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 2.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

fn fill(order_client_id: &str) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some(order_client_id.into()),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 6,
    }
}

fn gone_order_client_ids(p: &RestorePlan) -> Vec<&str> {
    p.gone.iter().map(|r| r.order_client_id.as_str()).collect()
}

fn adopt_order_client_ids(p: &RestorePlan) -> Vec<&str> {
    p.adopt.iter().map(|r| r.order_client_id.as_deref().unwrap()).collect()
}

#[test]
fn a_restored_order_client_id_resting_on_the_venue_is_adopted_with_its_report() {
    let rep = report(Some("c1"), "BTCUSDT", OrderStatus::Accepted);
    let p = plan_c1(std::slice::from_ref(&rep), &[]);
    assert_eq!(p.adopt, vec![rep]);
    assert!(p.gone.is_empty() && p.filled_while_down.is_empty());
}

#[test]
fn a_partially_filled_report_is_live_and_adopted() {
    let p = plan_c1(&[report(Some("c1"), "BTCUSDT", OrderStatus::PartiallyFilled)], &[]);
    assert_eq!(adopt_order_client_ids(&p), ["c1"]);
}

#[test]
fn a_venue_order_the_file_does_not_know_is_external_and_untouched() {
    let p = plan_c1(
        &[
            report(Some("c1"), "BTCUSDT", OrderStatus::Accepted),
            report(Some("someone-else"), "BTCUSDT", OrderStatus::Accepted),
            report(None, "BTCUSDT", OrderStatus::Accepted),
        ],
        &[],
    );
    assert_eq!(adopt_order_client_ids(&p), ["c1"], "only the restored order client id is adopted");
    assert!(p.gone.is_empty() && p.filled_while_down.is_empty());
}

#[test]
fn a_terminal_filled_report_is_filled_while_down_never_adopted_nor_gone() {
    let p = plan_c1(&[report(Some("c1"), "BTCUSDT", OrderStatus::Filled)], &[]);
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn every_other_terminal_report_makes_the_ref_gone_and_is_never_adopted() {
    for status in
        [OrderStatus::Canceled, OrderStatus::Rejected, OrderStatus::Expired, OrderStatus::Denied]
    {
        let p = plan_c1(&[report(Some("c1"), "BTCUSDT", status)], &[]);
        assert_eq!(gone_order_client_ids(&p), ["c1"], "{status:?}");
        assert!(p.adopt.is_empty() && p.filled_while_down.is_empty(), "{status:?}");
    }
}

#[test]
fn a_terminal_report_proves_gone_even_for_a_symbol_the_scope_does_not_list() {
    let p = plan_restore(
        &scope(&["ETHUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::Canceled)],
        &[],
    );
    assert_eq!(gone_order_client_ids(&p), ["c1"]);
}

#[test]
fn a_terminal_report_does_not_declare_a_ref_without_venue_and_symbol_gone() {
    let no_symbol = RestoredOrderRef { symbol: None, ..rf("c1", "BTCUSDT") };
    let no_venue = RestoredOrderRef { venue: None, ..rf("c2", "BTCUSDT") };
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[no_symbol, no_venue],
        &[
            report(Some("c1"), "BTCUSDT", OrderStatus::Canceled),
            report(Some("c2"), "BTCUSDT", OrderStatus::Canceled),
        ],
        &[],
    );
    assert_eq!(p, RestorePlan::default());
}

/// **Silence is UNPROVEN, and only counted quiet passes make it gone** (the audit's G2): the venue
/// not listing a restored order proves nothing on its own, so the first absent verdict (and each one
/// until the caller has counted [`RESTORE_QUIET_PASSES`]) keeps the ref, tells nobody, and lands it
/// in `unproven`.
#[test]
fn a_restored_order_client_id_absent_from_the_venue_is_unproven_until_the_quiet_passes_are_counted()
{
    for passes in 0..RESTORE_QUIET_PASSES {
        let r = RestoredOrderRef { quiet_passes: passes, ..rf("c1", "BTCUSDT") };
        let p = plan_restore(&scope(&["BTCUSDT"]), std::slice::from_ref(&r), &[], &[]);
        assert_eq!(p.unproven, std::slice::from_ref(&r), "{passes} quiet passes");
        assert!(p.gone.is_empty() && p.adopt.is_empty() && p.filled_while_down.is_empty());
    }
    let counted = quiet(rf("c1", "BTCUSDT"));
    let p = plan_restore(&scope(&["BTCUSDT"]), std::slice::from_ref(&counted), &[], &[]);
    assert_eq!(p.gone, std::slice::from_ref(&counted), "the count is reached");
    assert!(p.unproven.is_empty() && p.adopt.is_empty() && p.filled_while_down.is_empty());
}

#[test]
fn an_empty_order_report_declares_every_in_scope_ref_unproven_and_the_counted_ones_gone() {
    let refs = [quiet(rf("c1", "BTCUSDT")), rf("c2", "ETHUSDT"), quiet(rf("c3", "BTCUSDT"))];
    let p = plan_restore(&scope(&["BTCUSDT", "ETHUSDT"]), &refs, &[], &[]);
    assert_eq!(gone_order_client_ids(&p), ["c1", "c3"]);
    assert_eq!(unproven_order_client_ids(&p), ["c2"]);
}

#[test]
fn a_symbol_the_client_does_not_cover_is_never_declared_gone() {
    let p = plan_restore(&scope(&["ETHUSDT"]), &[rf("c1", "BTCUSDT")], &[], &[]);
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn an_empty_symbol_scope_declares_nothing_gone() {
    let p = plan_restore(&scope(&[]), &[rf("c1", "BTCUSDT")], &[], &[]);
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn another_venue_or_another_account_is_never_declared_gone() {
    let other_venue = RestoredOrderRef { venue: Some("bybit".into()), ..rf("c1", "BTCUSDT") };
    let other_account = RestoredOrderRef { account: Some("sub2".into()), ..rf("c2", "BTCUSDT") };
    let p = plan_restore(&scope(&["BTCUSDT"]), &[other_venue, other_account], &[], &[]);
    assert_eq!(p, RestorePlan::default());

    // and the mirror: the scope has an account, the ref has none (or a different one).
    let sc = RestoreScope { account: Some("main".into()), ..scope(&["BTCUSDT"]) };
    let no_account = quiet(rf("c3", "BTCUSDT"));
    let wrong_account =
        quiet(RestoredOrderRef { account: Some("sub2".into()), ..rf("c4", "BTCUSDT") });
    let right_account =
        quiet(RestoredOrderRef { account: Some("main".into()), ..rf("c5", "BTCUSDT") });
    let p = plan_restore(&sc, &[no_account, wrong_account, right_account], &[], &[]);
    assert_eq!(gone_order_client_ids(&p), ["c5"]);
}

#[test]
fn a_ref_without_venue_or_symbol_is_never_gone_by_absence() {
    let no_symbol = RestoredOrderRef { symbol: None, ..rf("c1", "BTCUSDT") };
    let no_venue = RestoredOrderRef { venue: None, ..rf("c2", "BTCUSDT") };
    let p = plan_restore(&scope(&["BTCUSDT"]), &[no_symbol, no_venue], &[], &[]);
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn a_ref_without_venue_symbol_or_account_is_still_adopted_by_its_order_client_id() {
    let bare = RestoredOrderRef { order_client_id: "c1".into(), ..Default::default() };
    let sc = RestoreScope { account: Some("main".into()), ..scope(&["BTCUSDT"]) };
    let rep = report(Some("c1"), "BTCUSDT", OrderStatus::Accepted);
    let p = plan_restore(&sc, &[bare], std::slice::from_ref(&rep), &[]);
    assert_eq!(p.adopt, vec![rep]);
}

#[test]
fn a_ref_of_another_venue_or_account_is_not_adopted_by_a_matching_order_client_id() {
    let other_venue = RestoredOrderRef { venue: Some("bybit".into()), ..rf("c1", "BTCUSDT") };
    let other_account = RestoredOrderRef { account: Some("sub2".into()), ..rf("c2", "BTCUSDT") };
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[other_venue, other_account],
        &[
            report(Some("c1"), "BTCUSDT", OrderStatus::Accepted),
            report(Some("c2"), "BTCUSDT", OrderStatus::Accepted),
        ],
        &[],
    );
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn a_fill_in_the_window_without_a_live_report_is_filled_while_down_never_gone() {
    let p = plan_c1(&[], &[fill("c1")]);
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn a_fill_beats_a_terminal_non_filled_report() {
    let p = plan_c1(&[report(Some("c1"), "BTCUSDT", OrderStatus::Canceled)], &[fill("c1")]);
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn a_fill_does_not_stop_a_still_live_order_from_being_adopted() {
    let p = plan_c1(&[report(Some("c1"), "BTCUSDT", OrderStatus::PartiallyFilled)], &[fill("c1")]);
    assert_eq!(adopt_order_client_ids(&p), ["c1"]);
    assert!(p.filled_while_down.is_empty());
}

#[test]
fn a_fill_of_an_unknown_order_client_id_or_without_an_order_client_id_changes_nothing() {
    let mut anon = fill("x");
    anon.order_client_id = None;
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[quiet(rf("c1", "BTCUSDT"))],
        &[],
        &[fill("someone-else"), anon],
    );
    assert_eq!(gone_order_client_ids(&p), ["c1"]);
}

#[test]
fn the_live_report_wins_over_a_terminal_duplicate_of_the_same_order_client_id() {
    let p = plan_c1(
        &[
            report(Some("c1"), "BTCUSDT", OrderStatus::Canceled),
            report(Some("c1"), "BTCUSDT", OrderStatus::Accepted),
        ],
        &[],
    );
    assert_eq!(adopt_order_client_ids(&p), ["c1"]);
    assert!(p.gone.is_empty());
}

#[test]
fn output_is_sorted_by_order_client_id_whatever_the_input_order() {
    let refs = [
        quiet(rf("c3", "BTCUSDT")),
        rf("c1", "BTCUSDT"),
        quiet(rf("c2", "BTCUSDT")),
        rf("c0", "BTCUSDT"),
        quiet(rf("c4", "BTCUSDT")),
    ];
    let reps = [
        report(Some("c2"), "BTCUSDT", OrderStatus::Accepted),
        report(Some("c0"), "BTCUSDT", OrderStatus::Accepted),
    ];
    let forward = plan_restore(&scope(&["BTCUSDT"]), &refs, &reps, &[]);
    assert_eq!(adopt_order_client_ids(&forward), ["c0", "c2"]);
    assert_eq!(gone_order_client_ids(&forward), ["c3", "c4"]);
    assert_eq!(unproven_order_client_ids(&forward), ["c1"]);

    let mut refs_rev = refs.clone();
    refs_rev.reverse();
    let mut reps_rev = reps.clone();
    reps_rev.reverse();
    assert_eq!(plan_restore(&scope(&["BTCUSDT"]), &refs_rev, &reps_rev, &[]), forward);
}

#[test]
fn each_ref_lands_in_at_most_one_list_and_a_repeated_order_client_id_is_judged_once() {
    let refs = [
        rf("adopted", "BTCUSDT"),
        quiet(rf("gone", "BTCUSDT")),
        rf("silent", "BTCUSDT"),
        rf("filled", "BTCUSDT"),
        rf("filled", "BTCUSDT"), // repeated order client id
        rf("cancelled", "BTCUSDT"),
        rf("untouched", "SOLUSDT"), // symbol the client does not cover
    ];
    let reps = [
        report(Some("adopted"), "BTCUSDT", OrderStatus::Accepted),
        report(Some("cancelled"), "BTCUSDT", OrderStatus::Canceled),
    ];
    let p = plan_restore(&scope(&["BTCUSDT"]), &refs, &reps, &[fill("filled")]);
    assert_eq!(adopt_order_client_ids(&p), ["adopted"]);
    assert_eq!(gone_order_client_ids(&p), ["cancelled", "gone"]);
    assert_eq!(unproven_order_client_ids(&p), ["silent"]);
    assert_eq!(p.filled_while_down, ["filled"]);

    let mut all: Vec<&str> = adopt_order_client_ids(&p);
    all.extend(gone_order_client_ids(&p));
    all.extend(unproven_order_client_ids(&p));
    all.extend(p.filled_while_down.iter().map(String::as_str));
    let unique: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(
        all.len(),
        unique.len(),
        "no order client id in two lists (or twice in one): {all:?}"
    );
    assert!(!all.contains(&"untouched"));
}

#[test]
fn no_refs_means_an_empty_plan_whatever_the_venue_reports() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::Accepted)],
        &[fill("c1")],
    );
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn the_gone_reason_text_is_pinned() {
    assert_eq!(RESTORE_GONE_REASON, "restart: absent at venue");
}

// ---- the kind-coverage claim (`ReconcileClient::lists_trigger_orders`) ---------------------------------

/// **Silence about a TRIGGER kind is no answer unless the client claims to list it.** Without the
/// claim a stop the report does not list may rest where the client does not look (okx's algo list),
/// so it stays `unproven` however many quiet passes were counted, neither gone nor adopted; with the
/// claim it is gone, once the passes are counted.
#[test]
fn an_absent_trigger_order_is_unproven_unless_the_client_lists_trigger_orders() {
    use vike_model::OrderType::{Stop, StopLimit, TakeProfit};
    let claiming = RestoreScope { lists_trigger_orders: true, ..scope(&["BTCUSDT"]) };
    for kind in [Stop, StopLimit, TakeProfit] {
        let r = quiet(rf_of("c1", "BTCUSDT", kind.clone()));
        let held = plan_restore(&scope(&["BTCUSDT"]), std::slice::from_ref(&r), &[], &[]);
        assert_eq!(
            held.unproven,
            std::slice::from_ref(&r),
            "{kind:?}: held for good, the client makes no claim"
        );
        assert!(held.gone.is_empty() && held.adopt.is_empty() && held.filled_while_down.is_empty());

        let judged = plan_restore(&claiming, std::slice::from_ref(&r), &[], &[]);
        assert_eq!(
            judged.gone,
            std::slice::from_ref(&r),
            "{kind:?}: gone where the client lists trigger orders"
        );
        assert!(judged.unproven.is_empty());

        let early = RestoredOrderRef { quiet_passes: 0, ..r };
        let uncounted = plan_restore(&claiming, std::slice::from_ref(&early), &[], &[]);
        assert_eq!(
            uncounted.unproven,
            [early],
            "{kind:?}: even a claim waits for the quiet passes"
        );
    }
}

/// A plain kind is judged as a plain order whatever the claim says: the claim is about trigger kinds
/// only, so once the quiet passes are counted silence makes it gone with or without it.
#[test]
fn a_plain_kind_is_gone_on_counted_silence_with_or_without_the_claim() {
    use vike_model::OrderType::{Limit, Market};
    for claim in [false, true] {
        let sc = RestoreScope { lists_trigger_orders: claim, ..scope(&["BTCUSDT"]) };
        let refs = [quiet(rf_of("c1", "BTCUSDT", Limit)), quiet(rf_of("c2", "BTCUSDT", Market))];
        let p = plan_restore(&sc, &refs, &[], &[]);
        assert_eq!(gone_order_client_ids(&p), ["c1", "c2"], "claim {claim}");
        assert!(p.unproven.is_empty(), "claim {claim}");
    }
}

/// **A ref that records NO kind may be a stop** (a line written before the terms existed), so on a
/// client that does not claim to list trigger orders its silence is no proof, however many quiet
/// passes were counted: it stays `unproven` (an old plain limit on such a venue waits for the
/// ownership file's 30-day TTL). With the claim the venue's list covers every kind and silence counts.
#[test]
fn a_ref_with_no_recorded_kind_is_gone_on_counted_silence_only_where_the_client_claims() {
    let old = quiet(rf_unrecorded("c1", "BTCUSDT"));
    let held = plan_restore(&scope(&["BTCUSDT"]), std::slice::from_ref(&old), &[], &[]);
    assert_eq!(held.unproven, std::slice::from_ref(&old), "no claim: held for good");
    assert!(held.gone.is_empty());

    let claiming = RestoreScope { lists_trigger_orders: true, ..scope(&["BTCUSDT"]) };
    let judged = plan_restore(&claiming, std::slice::from_ref(&old), &[], &[]);
    assert_eq!(judged.gone, std::slice::from_ref(&old), "claim: gone on counted silence");
    assert!(judged.unproven.is_empty());
}

/// The claim only governs SILENCE. A listed trigger order is adopted, a terminal report proves it
/// closed and a fill is still a fill, whether or not the client claims anything.
#[test]
fn the_claim_does_not_change_what_a_report_or_a_fill_proves() {
    use vike_model::OrderType::Stop;
    let live = report(Some("live"), "BTCUSDT", OrderStatus::Accepted);
    let dead = report(Some("dead"), "BTCUSDT", OrderStatus::Canceled);
    let refs = [
        rf_of("live", "BTCUSDT", Stop),
        rf_of("dead", "BTCUSDT", Stop),
        rf_of("filled", "BTCUSDT", Stop),
    ];
    let p = plan_restore(&scope(&["BTCUSDT"]), &refs, &[live, dead], &[fill("filled")]);
    assert_eq!(adopt_order_client_ids(&p), ["live"]);
    assert_eq!(gone_order_client_ids(&p), ["dead"]);
    assert_eq!(p.filled_while_down, ["filled"]);
    assert!(p.unproven.is_empty());
}

// ---- the venue-id join of a fill (the audit's G2) --------------------------------------------------

/// A fill row of the order the venue knows as `venue_order_id`, carrying NO order client id: what binance,
/// aster, cTrader, IG and Alpaca report, and what a triggered stop's child order looks like.
fn order_client_id_less_fill(venue_order_id: &str, symbol: &str) -> FillReport {
    FillReport {
        order_client_id: None,
        venue_order_id: venue_order_id.into(),
        symbol: symbol.into(),
        ..fill("unused")
    }
}

/// **A fill that carries no order client id joins its order by the venue order id** the ownership file learned
/// at the acknowledgement: the ref is `filled_while_down`, neither gone nor unproven, so the false
/// "cancelled" the strategy used to hear (and the forgotten ownership) cannot happen.
#[test]
fn an_order_client_id_less_fill_joins_its_order_by_the_acknowledged_venue_order_id() {
    let r = rf_acked("c1", "BTCUSDT", "7001");
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[r],
        &[],
        &[order_client_id_less_fill("7001", "BTCUSDT")],
    );
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.gone.is_empty() && p.unproven.is_empty() && p.adopt.is_empty());

    // …even when the ref has already counted its quiet passes: a fill is proof, silence is not
    let counted = quiet(rf_acked("c2", "BTCUSDT", "7002"));
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[counted],
        &[],
        &[order_client_id_less_fill("7002", "BTCUSDT")],
    );
    assert_eq!(p.filled_while_down, ["c2"]);
    assert!(p.gone.is_empty());
}

/// The join is by the id AND the symbol (binance's order ids are per symbol), only for a ref that
/// knows its id, and an empty id joins nothing: a fill of ANOTHER order must not spend this one.
#[test]
fn the_venue_id_join_needs_the_same_id_the_same_symbol_and_a_known_id() {
    let fills = [
        order_client_id_less_fill("7001", "ETHUSDT"),
        order_client_id_less_fill("9999", "BTCUSDT"),
    ];
    let wrong_symbol = rf_acked("c1", "BTCUSDT", "7001");
    let not_acked = rf("c2", "BTCUSDT"); // learned no id: the fill below cannot be its own
    let other_id = rf_acked("c3", "BTCUSDT", "7003");
    let p = plan_restore(&scope(&["BTCUSDT"]), &[wrong_symbol, not_acked, other_id], &[], &fills);
    assert!(p.filled_while_down.is_empty(), "{p:?}");
    assert_eq!(unproven_order_client_ids(&p), ["c1", "c2", "c3"]);

    let empty_id = RestoredOrderRef { venue_order_id: Some(String::new()), ..rf("c4", "BTCUSDT") };
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[empty_id],
        &[],
        &[order_client_id_less_fill("", "BTCUSDT")],
    );
    assert!(p.filled_while_down.is_empty(), "an empty id joins nothing: {p:?}");
}

/// A ref with NO symbol cannot say a fill is its own (the id is per symbol on binance): the join
/// needs the symbol on both sides, so a fill of any symbol with the same id leaves the ref alone.
#[test]
fn a_ref_without_a_symbol_is_never_joined_to_a_fill_by_id() {
    let no_symbol = RestoredOrderRef { symbol: None, ..rf_acked("c1", "BTCUSDT", "7001") };
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[no_symbol],
        &[],
        &[order_client_id_less_fill("7001", "BTCUSDT")],
    );
    assert_eq!(p, RestorePlan::default(), "untouched, as a ref without a symbol is by absence");
}

/// The id join changes nothing a report decides: a live report still adopts, a terminal one still
/// proves gone (a fill beating a terminal report is the existing order client id rule, now by id too).
#[test]
fn the_venue_id_join_leaves_a_live_report_adopting_and_beats_a_terminal_one() {
    let live = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf_acked("c1", "BTCUSDT", "7001")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::PartiallyFilled)],
        &[order_client_id_less_fill("7001", "BTCUSDT")],
    );
    assert_eq!(adopt_order_client_ids(&live), ["c1"]);

    let canceled = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf_acked("c1", "BTCUSDT", "7001")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::Canceled)],
        &[order_client_id_less_fill("7001", "BTCUSDT")],
    );
    assert_eq!(canceled.filled_while_down, ["c1"]);
    assert!(canceled.gone.is_empty());
}

// --- a report that names NO order client id, joined to its restored order by the acknowledged venue order id
// --- (`name_by_venue_order_id`): what fxcm and IG list after a restart of both daemons.

/// A `venue` scope over `symbol`, and a restored limit `order_client_id` on it that the venue acknowledged as `venue_id`.
fn venue_case(
    venue: &str,
    symbol: &str,
    order_client_id: &str,
    venue_id: &str,
) -> (RestoreScope, RestoredOrderRef) {
    let scope =
        RestoreScope { venue: venue.into(), symbols: vec![symbol.into()], ..Default::default() };
    let r = RestoredOrderRef {
        venue: Some(venue.into()),
        ..rf_acked(order_client_id, symbol, venue_id)
    };
    (scope, r)
}

/// A resting row of `venue` on `symbol` that names no order client id, under the venue's id `venue_id`.
fn order_client_id_less_row(venue: &str, symbol: &str, venue_id: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: venue.into(),
        venue_order_id: venue_id.into(),
        ..report(None, symbol, OrderStatus::Accepted)
    }
}

/// **fxcm and IG after a restart of both daemons**: the Orders table (fxcm, a numeric order id) and the working orders
/// (IG, a `dealId`) list a resting order of ours with no order client id. The join names it by the venue id its ownership line
/// learned, so the planner ADOPTS it under our order client id (and the report the caller diffs carries the order client id too).
#[test]
fn an_order_client_id_less_report_of_the_acknowledged_venue_id_is_adopted_under_our_order_client_id()
 {
    for (venue, symbol, venue_id) in
        [("fxcm", "EURUSD", "416262101"), ("ig", "CS.D.EURUSD.MINI.IP", "DIAAA9")]
    {
        let (scope, r) = venue_case(venue, symbol, "c-ours", venue_id);
        let mut rows = vec![order_client_id_less_row(venue, symbol, venue_id)];
        assert_eq!(
            name_by_venue_order_id(&scope, std::slice::from_ref(&r), &mut rows),
            1,
            "{venue}"
        );
        assert_eq!(
            rows[0].order_client_id.as_deref(),
            Some("c-ours"),
            "{venue}: the diff's row is ours"
        );
        let p = plan_restore(&scope, &[r], &rows, &[]);
        assert_eq!(adopt_order_client_ids(&p), ["c-ours"], "{venue}");
        assert_eq!(
            p.adopt[0].venue_order_id.as_str(),
            venue_id,
            "{venue}: the id the client is taught"
        );
    }
}

/// A row whose venue id matches no restored order stays FOREIGN (no order client id, nothing adopted), and the restored order
/// it is not stays merely unproven: a stranger's order is never taken for ours.
#[test]
fn an_order_client_id_less_report_of_another_venue_id_stays_foreign() {
    let (scope, r) = venue_case("fxcm", "EURUSD", "c-ours", "416262101");
    let mut rows = vec![order_client_id_less_row("fxcm", "EURUSD", "416262199")];
    assert_eq!(name_by_venue_order_id(&scope, std::slice::from_ref(&r), &mut rows), 0);
    assert_eq!(rows[0].order_client_id, None, "a foreign order keeps no order client id");
    let p = plan_restore(&scope, &[r], &rows, &[]);
    assert!(p.adopt.is_empty());
    assert_eq!(unproven_order_client_ids(&p), ["c-ours"]);
}

/// Nothing is guessed: another symbol is another order, an id two restored orders claim names neither, a ref of
/// another venue is not this pass's, a row that names an order client id keeps it, and a ref whose order client id a row already names
/// (a bridge that labels its rows) is not joined to a second row.
#[test]
fn the_venue_id_join_guesses_nothing() {
    let (scope, r) = venue_case("fxcm", "EURUSD", "c1", "4001");
    let named = |refs: &[RestoredOrderRef], mut rows: Vec<OrderStatusReport>| {
        let n = name_by_venue_order_id(&scope, refs, &mut rows);
        (n, rows.into_iter().map(|o| o.order_client_id).collect::<Vec<_>>())
    };
    // another symbol
    assert_eq!(
        named(std::slice::from_ref(&r), vec![order_client_id_less_row("fxcm", "GBPUSD", "4001")]),
        (0, vec![None])
    );
    // two refs claim the id
    let twin = RestoredOrderRef { order_client_id: "c2".into(), ..r.clone() };
    assert_eq!(
        named(&[r.clone(), twin], vec![order_client_id_less_row("fxcm", "EURUSD", "4001")]),
        (0, vec![None])
    );
    // another venue's ref
    let bybit = RestoredOrderRef { venue: Some("bybit".into()), ..r.clone() };
    assert_eq!(
        named(&[bybit], vec![order_client_id_less_row("fxcm", "EURUSD", "4001")]),
        (0, vec![None])
    );
    // a row that names an order client id keeps it
    let labelled = OrderStatusReport {
        order_client_id: Some("someone".into()),
        ..order_client_id_less_row("fxcm", "EURUSD", "4001")
    };
    assert_eq!(
        named(std::slice::from_ref(&r), vec![labelled]),
        (0, vec![Some("someone".to_string())])
    );
    // the bridge already named c1 on its own row: a second, order-client-id-less row of the same id is not joined to it
    let ours = OrderStatusReport {
        order_client_id: Some("c1".into()),
        ..order_client_id_less_row("fxcm", "EURUSD", "4001")
    };
    assert_eq!(
        named(
            std::slice::from_ref(&r),
            vec![ours, order_client_id_less_row("fxcm", "EURUSD", "4001")]
        ),
        (0, vec![Some("c1".to_string()), None])
    );
}

/// A trigger ref the client does not cover (another symbol, venue or account) is untouched, not
/// held: `unproven` is for what THIS pass could have judged.
#[test]
fn an_out_of_scope_trigger_ref_is_untouched_not_unproven() {
    use vike_model::OrderType::Stop;
    let other_venue =
        RestoredOrderRef { venue: Some("bybit".into()), ..rf_of("c1", "BTCUSDT", Stop) };
    let p =
        plan_restore(&scope(&["ETHUSDT"]), &[rf_of("c2", "BTCUSDT", Stop), other_venue], &[], &[]);
    assert_eq!(p, RestorePlan::default());
}
