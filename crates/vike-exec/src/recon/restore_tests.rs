use super::*;
use vike_model::events::LiquiditySide;

fn scope(symbols: &[&str]) -> RestoreScope {
    RestoreScope {
        venue: "binance".into(),
        account: None,
        symbols: symbols.iter().map(|s| s.to_string()).collect(),
    }
}

/// A full ref: venue + symbol + no account, the shape a current ownership record has.
fn rf(coid: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.into(),
        venue: Some("binance".into()),
        symbol: Some(symbol.into()),
        account: None,
    }
}

fn report(coid: Option<&str>, symbol: &str, status: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        venue_order_id: "v1".into(),
        client_order_id: coid.map(str::to_string),
        side: 1,
        order_type: "limit".into(),
        qty: 2.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: status.into(),
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
    let rep = report(Some("c1"), "BTCUSDT", "ACCEPTED");
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
        &[report(Some("c1"), "BTCUSDT", "PARTIALLY_FILLED")],
        &[],
    );
    assert_eq!(adopt_coids(&p), ["c1"]);
}

#[test]
fn an_unparsable_status_counts_as_live_like_reregister_orders() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "WEIRD_VENUE_STATE", "WEIRD_VENUE_STATE")],
        &[],
    );
    assert_eq!(adopt_coids(&p), ["c1"]);
    assert!(p.gone.is_empty());
}

#[test]
fn a_venue_order_the_file_does_not_know_is_external_and_untouched() {
    let p = plan_restore(
        &scope(&["BTCUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[
            report(Some("c1"), "BTCUSDT", "ACCEPTED"),
            report(Some("someone-else"), "BTCUSDT", "ACCEPTED"),
            report(None, "BTCUSDT", "ACCEPTED"),
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
        &[report(Some("c1"), "BTCUSDT", "FILLED")],
        &[],
    );
    assert_eq!(p.filled_while_down, ["c1"]);
    assert!(p.adopt.is_empty() && p.gone.is_empty());
}

#[test]
fn every_other_terminal_report_makes_the_ref_gone_and_is_never_adopted() {
    for status in ["CANCELED", "REJECTED", "EXPIRED", "DENIED"] {
        let p = plan_restore(
            &scope(&["BTCUSDT"]),
            &[rf("c1", "BTCUSDT")],
            &[report(Some("c1"), "BTCUSDT", status)],
            &[],
        );
        assert_eq!(gone_coids(&p), ["c1"], "{status}");
        assert!(p.adopt.is_empty() && p.filled_while_down.is_empty(), "{status}");
    }
}

#[test]
fn a_terminal_report_proves_gone_even_for_a_symbol_the_scope_does_not_list() {
    let p = plan_restore(
        &scope(&["ETHUSDT"]),
        &[rf("c1", "BTCUSDT")],
        &[report(Some("c1"), "BTCUSDT", "CANCELED")],
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
        &[report(Some("c1"), "BTCUSDT", "CANCELED"), report(Some("c2"), "BTCUSDT", "CANCELED")],
        &[],
    );
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn a_restored_coid_absent_from_the_venue_is_gone() {
    let p = plan_restore(&scope(&["BTCUSDT"]), &[rf("c1", "BTCUSDT")], &[], &[]);
    assert_eq!(p.gone, [rf("c1", "BTCUSDT")]);
    assert!(p.adopt.is_empty() && p.filled_while_down.is_empty());
}

#[test]
fn an_empty_order_report_declares_every_in_scope_ref_gone() {
    let refs = [rf("c1", "BTCUSDT"), rf("c2", "ETHUSDT"), rf("c3", "BTCUSDT")];
    let p = plan_restore(&scope(&["BTCUSDT", "ETHUSDT"]), &refs, &[], &[]);
    assert_eq!(gone_coids(&p), ["c1", "c2", "c3"]);
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
    let no_account = rf("c3", "BTCUSDT");
    let wrong_account = RestoredOrderRef { account: Some("sub2".into()), ..rf("c4", "BTCUSDT") };
    let right_account = RestoredOrderRef { account: Some("main".into()), ..rf("c5", "BTCUSDT") };
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
    let bare = RestoredOrderRef { coid: "c1".into(), venue: None, symbol: None, account: None };
    let sc = RestoreScope { account: Some("main".into()), ..scope(&["BTCUSDT"]) };
    let rep = report(Some("c1"), "BTCUSDT", "ACCEPTED");
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
        &[report(Some("c1"), "BTCUSDT", "ACCEPTED"), report(Some("c2"), "BTCUSDT", "ACCEPTED")],
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
        &[report(Some("c1"), "BTCUSDT", "CANCELED")],
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
        &[report(Some("c1"), "BTCUSDT", "PARTIALLY_FILLED")],
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
        &[rf("c1", "BTCUSDT")],
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
        &[report(Some("c1"), "BTCUSDT", "CANCELED"), report(Some("c1"), "BTCUSDT", "ACCEPTED")],
        &[],
    );
    assert_eq!(adopt_coids(&p), ["c1"]);
    assert!(p.gone.is_empty());
}

#[test]
fn output_is_sorted_by_coid_whatever_the_input_order() {
    let refs = [rf("c3", "BTCUSDT"), rf("c1", "BTCUSDT"), rf("c2", "BTCUSDT"), rf("c0", "BTCUSDT")];
    let reps =
        [report(Some("c2"), "BTCUSDT", "ACCEPTED"), report(Some("c0"), "BTCUSDT", "ACCEPTED")];
    let forward = plan_restore(&scope(&["BTCUSDT"]), &refs, &reps, &[]);
    assert_eq!(adopt_coids(&forward), ["c0", "c2"]);
    assert_eq!(gone_coids(&forward), ["c1", "c3"]);

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
        rf("gone", "BTCUSDT"),
        rf("filled", "BTCUSDT"),
        rf("filled", "BTCUSDT"), // repeated coid
        rf("cancelled", "BTCUSDT"),
        rf("untouched", "SOLUSDT"), // symbol the client does not cover
    ];
    let reps = [
        report(Some("adopted"), "BTCUSDT", "ACCEPTED"),
        report(Some("cancelled"), "BTCUSDT", "CANCELED"),
    ];
    let p = plan_restore(&scope(&["BTCUSDT"]), &refs, &reps, &[fill("filled")]);
    assert_eq!(adopt_coids(&p), ["adopted"]);
    assert_eq!(gone_coids(&p), ["cancelled", "gone"]);
    assert_eq!(p.filled_while_down, ["filled"]);

    let mut all: Vec<&str> = adopt_coids(&p);
    all.extend(gone_coids(&p));
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
        &[report(Some("c1"), "BTCUSDT", "ACCEPTED")],
        &[fill("c1")],
    );
    assert_eq!(p, RestorePlan::default());
}

#[test]
fn the_gone_reason_text_is_pinned() {
    assert_eq!(RESTORE_GONE_REASON, "restart: absent at venue");
}
