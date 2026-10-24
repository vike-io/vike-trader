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
fn rf(coid: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.into(),
        venue: Some("binance".into()),
        symbol: Some(symbol.into()),
        account: None,
        order_type: Some(vike_model::OrderType::Limit),
        ..Default::default()
    }
}

/// [`rf`] of a line written before the kind was recorded.
fn rf_unrecorded(coid: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef { order_type: None, ..rf(coid, symbol) }
}

/// [`rf`] recording that the order was of `kind`.
fn rf_of(coid: &str, symbol: &str, kind: vike_model::OrderType) -> RestoredOrderRef {
    RestoredOrderRef { order_type: Some(kind), ..rf(coid, symbol) }
}

/// `r` after the caller counted [`RESTORE_QUIET_PASSES`] quiet passes for it: silence now proves it
/// gone (for a kind the client can speak for).
fn quiet(r: RestoredOrderRef) -> RestoredOrderRef {
    RestoredOrderRef { quiet_passes: RESTORE_QUIET_PASSES, ..r }
}

/// [`rf`] that remembers the venue acknowledged it as `venue_order_id`.
fn rf_acked(coid: &str, symbol: &str, venue_order_id: &str) -> RestoredOrderRef {
    RestoredOrderRef { venue_order_id: Some(venue_order_id.into()), ..rf(coid, symbol) }
}

fn unproven_coids(p: &RestorePlan) -> Vec<&str> {
    p.unproven.iter().map(|r| r.coid.as_str()).collect()
}

fn report(coid: Option<&str>, symbol: &str, status: OrderStatus) -> OrderStatusReport {
    OrderStatusReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        venue_order_id: "v1".into(),
        client_order_id: coid.map(str::to_string),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 2.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status,
        ts: 5,
    }
}

fn fill(coid: &str) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v1".into(),
        client_order_id: Some(coid.into()),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 6,
    }
}

fn gone_coids(p: &RestorePlan) -> Vec<&str> {
    p.gone.iter().map(|r| r.coid.as_str()).collect()
}

fn adopt_coids(p: &RestorePlan) -> Vec<&str> {
    p.adopt.iter().map(|r| r.client_order_id.as_deref().unwrap()).collect()
}

#[test]
fn a_restored_coid_resting_on_the_venue_is_adopted_with_its_report() {
    let rep = report(Some("c1"), "BTCUSDT", OrderStatus::Accepted);
    let p =
        plan_restore(&scope(&["BTCUSDT"]), &[rf("c1", "BTCUSDT")], std::slice::from_ref(&rep), &[]);
    assert_eq!(p.adopt, vec![rep]);
    assert!(p.gone.is_empty() && p.filled_while_down.is_empty());
}

#[test]
fn a_partially_filled_report_is_live_and_adopted() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::PartiallyFilled)],
        &[],
    );
    assert_eq!(adopt_coids(&p), ["c1"]);
}

#[test]
fn a_venue_order_the_file_does_not_know_is_external_and_untouched() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[
            report(Some("c1"), "BTCUSDT", OrderStatus::Accepted),
            report(Some("someone-else"), "BTCUSDT", OrderStatus::Accepted),
            report(None, "BTCUSDT", OrderStatus::Accepted),
        ],
        &[],
    );
    assert_eq!(adopt_coids(&p), ["c1"], "only the restored coid is adopted");
    assert!(p.gone.is_empty() && p.filled_while_down.is_empty());
}

#[test]
fn a_terminal_filled_report_is_filled_while_down_never_adopted_nor_gone() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::Filled)],
        &[],
    );
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn every_other_terminal_report_makes_the_ref_gone_and_is_never_adopted() {
    for status in
        [OrderStatus::Canceled, OrderStatus::Rejected, OrderStatus::Expired, OrderStatus::Denied]
    {
        let p = plan_restore(
            &scope(&["BTCUSDT"]),
            &[rf("c1", "BTCUSDT")],
            &[report(Some("c1"), "BTCUSDT", status)],
            &[],
        );
        assert_eq!(gone_coids(&p), ["c1"], "{status:?}");
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
    assert_eq!(gone_coids(&p), ["c1"]);
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
fn a_restored_coid_absent_from_the_venue_is_unproven_until_the_quiet_passes_are_counted() {
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
    assert_eq!(gone_coids(&p), ["c1", "c3"]);
    assert_eq!(unproven_coids(&p), ["c2"]);
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
    assert_eq!(gone_coids(&p), ["c5"]);
}

#[test]
fn a_ref_without_venue_or_symbol_is_never_gone_by_absence() {
    let no_symbol = RestoredOrderRef { symbol: None, ..rf("c1", "BTCUSDT") };
    let no_venue = RestoredOrderRef { venue: None, ..rf("c2", "BTCUSDT") };
    let p = plan_restore(&scope(&["BTCUSDT"]), &[no_symbol, no_venue], &[], &[]);
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn a_ref_without_venue_symbol_or_account_is_still_adopted_by_its_coid() {
    let bare = RestoredOrderRef { coid: "c1".into(), ..Default::default() };
    let sc = RestoreScope { account: Some("main".into()), ..scope(&["BTCUSDT"]) };
    let rep = report(Some("c1"), "BTCUSDT", OrderStatus::Accepted);
    let p = plan_restore(&sc, &[bare], std::slice::from_ref(&rep), &[]);
    assert_eq!(p.adopt, vec![rep]);
}

#[test]
fn a_ref_of_another_venue_or_account_is_not_adopted_by_a_matching_coid() {
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
    let p = plan_restore(&scope(&["BTCUSDT"]), &[rf("c1", "BTCUSDT")], &[], &[fill("c1")]);
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn a_fill_beats_a_terminal_non_filled_report() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::Canceled)],
        &[fill("c1")],
    );
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn a_fill_does_not_stop_a_still_live_order_from_being_adopted() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::PartiallyFilled)],
        &[fill("c1")],
    );
    assert_eq!(adopt_coids(&p), ["c1"]);
    assert!(p.filled_while_down.is_empty());
}

#[test]
fn a_fill_of_an_unknown_coid_or_without_a_coid_changes_nothing() {
    let mut anon = fill("x");
    anon.client_order_id = None;
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[quiet(rf("c1", "BTCUSDT"))],
        &[],
        &[fill("someone-else"), anon],
    );
    assert_eq!(gone_coids(&p), ["c1"]);
}

#[test]
fn the_live_report_wins_over_a_terminal_duplicate_of_the_same_coid() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[
            report(Some("c1"), "BTCUSDT", OrderStatus::Canceled),
            report(Some("c1"), "BTCUSDT", OrderStatus::Accepted),
        ],
        &[],
    );
    assert_eq!(adopt_coids(&p), ["c1"]);
    assert!(p.gone.is_empty());
}

#[test]
fn output_is_sorted_by_coid_whatever_the_input_order() {
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
    assert_eq!(adopt_coids(&forward), ["c0", "c2"]);
    assert_eq!(gone_coids(&forward), ["c3", "c4"]);
    assert_eq!(unproven_coids(&forward), ["c1"]);

    let mut refs_rev = refs.clone();
    refs_rev.reverse();
    let mut reps_rev = reps.clone();
    reps_rev.reverse();
    assert_eq!(plan_restore(&scope(&["BTCUSDT"]), &refs_rev, &reps_rev, &[]), forward);
}

#[test]
fn each_ref_lands_in_at_most_one_list_and_a_repeated_coid_is_judged_once() {
    let refs = [
        rf("adopted", "BTCUSDT"),
        quiet(rf("gone", "BTCUSDT")),
        rf("silent", "BTCUSDT"),
        rf("filled", "BTCUSDT"),
        rf("filled", "BTCUSDT"), // repeated coid
        rf("cancelled", "BTCUSDT"),
        rf("untouched", "SOLUSDT"), // symbol the client does not cover
    ];
    let reps = [
        report(Some("adopted"), "BTCUSDT", OrderStatus::Accepted),
        report(Some("cancelled"), "BTCUSDT", OrderStatus::Canceled),
    ];
    let p = plan_restore(&scope(&["BTCUSDT"]), &refs, &reps, &[fill("filled")]);
    assert_eq!(adopt_coids(&p), ["adopted"]);
    assert_eq!(gone_coids(&p), ["cancelled", "gone"]);
    assert_eq!(unproven_coids(&p), ["silent"]);
    assert_eq!(p.filled_while_down, ["filled"]);

    let mut all: Vec<&str> = adopt_coids(&p);
    all.extend(gone_coids(&p));
    all.extend(unproven_coids(&p));
    all.extend(p.filled_while_down.iter().map(String::as_str));
    let unique: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(all.len(), unique.len(), "no coid in two lists (or twice in one): {all:?}");
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

// ---- the kind-coverage claim (`ReconClient::lists_trigger_orders`) ---------------------------------

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
        assert_eq!(gone_coids(&p), ["c1", "c2"], "claim {claim}");
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
    assert_eq!(adopt_coids(&p), ["live"]);
    assert_eq!(gone_coids(&p), ["dead"]);
    assert_eq!(p.filled_while_down, ["filled"]);
    assert!(p.unproven.is_empty());
}

// ---- the venue-id join of a fill (the audit's G2) --------------------------------------------------

/// A fill row of the order the venue knows as `venue_order_id`, carrying NO coid: what binance,
/// aster, cTrader, IG and Alpaca report, and what a triggered stop's child order looks like.
fn coidless_fill(venue_order_id: &str, symbol: &str) -> FillReport {
    FillReport {
        client_order_id: None,
        venue_order_id: venue_order_id.into(),
        symbol: symbol.into(),
        ..fill("unused")
    }
}

/// **A fill that carries no coid joins its order by the venue order id** the ownership file learned
/// at the acknowledgement: the ref is `filled_while_down`, neither gone nor unproven, so the false
/// "cancelled" the strategy used to hear (and the forgotten ownership) cannot happen.
#[test]
fn a_coidless_fill_joins_its_order_by_the_acknowledged_venue_order_id() {
    let r = rf_acked("c1", "BTCUSDT", "7001");
    let p = plan_restore(&scope(&["BTCUSDT"]), &[r], &[], &[coidless_fill("7001", "BTCUSDT")]);
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.gone.is_empty() && p.unproven.is_empty() && p.adopt.is_empty());

    // …even when the ref has already counted its quiet passes: a fill is proof, silence is not
    let counted = quiet(rf_acked("c2", "BTCUSDT", "7002"));
    let p =
        plan_restore(&scope(&["BTCUSDT"]), &[counted], &[], &[coidless_fill("7002", "BTCUSDT")]);
    assert_eq!(p.filled_while_down, ["c2"]);
    assert!(p.gone.is_empty());
}

/// The join is by the id AND the symbol (binance's order ids are per symbol), only for a ref that
/// knows its id, and an empty id joins nothing: a fill of ANOTHER order must not spend this one.
#[test]
fn the_venue_id_join_needs_the_same_id_the_same_symbol_and_a_known_id() {
    let fills = [coidless_fill("7001", "ETHUSDT"), coidless_fill("9999", "BTCUSDT")];
    let wrong_symbol = rf_acked("c1", "BTCUSDT", "7001");
    let not_acked = rf("c2", "BTCUSDT"); // learned no id: the fill below cannot be its own
    let other_id = rf_acked("c3", "BTCUSDT", "7003");
    let p = plan_restore(&scope(&["BTCUSDT"]), &[wrong_symbol, not_acked, other_id], &[], &fills);
    assert!(p.filled_while_down.is_empty(), "{p:?}");
    assert_eq!(unproven_coids(&p), ["c1", "c2", "c3"]);

    let empty_id = RestoredOrderRef { venue_order_id: Some(String::new()), ..rf("c4", "BTCUSDT") };
    let p = plan_restore(&scope(&["BTCUSDT"]), &[empty_id], &[], &[coidless_fill("", "BTCUSDT")]);
    assert!(p.filled_while_down.is_empty(), "an empty id joins nothing: {p:?}");
}

/// A ref with NO symbol cannot say a fill is its own (the id is per symbol on binance): the join
/// needs the symbol on both sides, so a fill of any symbol with the same id leaves the ref alone.
#[test]
fn a_ref_without_a_symbol_is_never_joined_to_a_fill_by_id() {
    let no_symbol = RestoredOrderRef { symbol: None, ..rf_acked("c1", "BTCUSDT", "7001") };
    let p =
        plan_restore(&scope(&["BTCUSDT"]), &[no_symbol], &[], &[coidless_fill("7001", "BTCUSDT")]);
    assert_eq!(p, RestorePlan::default(), "untouched, as a ref without a symbol is by absence");
}

/// The id join changes nothing a report decides: a live report still adopts, a terminal one still
/// proves gone (a fill beating a terminal report is the existing coid rule, now by id too).
#[test]
fn the_venue_id_join_leaves_a_live_report_adopting_and_beats_a_terminal_one() {
    let live = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf_acked("c1", "BTCUSDT", "7001")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::PartiallyFilled)],
        &[coidless_fill("7001", "BTCUSDT")],
    );
    assert_eq!(adopt_coids(&live), ["c1"]);

    let canceled = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf_acked("c1", "BTCUSDT", "7001")],
        &[report(Some("c1"), "BTCUSDT", OrderStatus::Canceled)],
        &[coidless_fill("7001", "BTCUSDT")],
    );
    assert_eq!(canceled.filled_while_down, ["c1"]);
    assert!(canceled.gone.is_empty());
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
