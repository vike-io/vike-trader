//! The IG `ReconClient` reads every epic the engine admitted on demand (decision 0173), beside the
//! mounted one, over a SCRIPTED LOOPBACK IG double (`fake_ig`): the real `IgReconClient` logs in against
//! `127.0.0.1` and the double answers by the request line, so no IG network is touched.
//!
//! What this pins: a pass with nothing admitted makes the requests it always made; the account-wide
//! bodies are read ONCE and parsed per epic (the mounted epic first); an admitted epic IG does not list is
//! a FLAT row; an epic nobody admitted is not reported; the client claims only what it reads; and a body
//! it cannot parse fails the WHOLE fetch (the restore planner reads an order a successful report lacks as
//! gone). The fills read is PAGED when (and only when) an epic is admitted: it follows
//! `metadata.paging.next` for at most ten requests and fails the pass rather than report a truncated
//! history. The bodies are hand-written in the documented shapes, never captured.

use serde_json::json;
use vike_exec::recon::ReconClient;
use vike_ig::IgReconClient;

use crate::fake_ig::{
    FakeIg, Route, accounts_route, activity_link, activity_pages, login_route, serve,
};

const MOUNTED: &str = "CS.D.EURUSD.MINI.IP";
const ADMITTED: &str = "CS.D.GBPUSD.MINI.IP";
const FOREIGN: &str = "CS.D.USDJPY.MINI.IP";

fn client(ig: &FakeIg) -> IgReconClient {
    IgReconClient::connect(&ig.config(), MOUNTED).expect("the double logs in")
}

fn working(deal: &str, epic: &str) -> serde_json::Value {
    json!({
        "workingOrderData": { "dealId": deal, "epic": epic, "direction": "BUY", "orderType": "LIMIT",
                              "orderSize": 1.0, "createdDateUTC": "2022-01-15T14:00:00" },
        "marketData": { "epic": epic }
    })
}

fn position(deal: &str, epic: &str, direction: &str, size: f64) -> serde_json::Value {
    json!({
        "position": { "dealId": deal, "direction": direction, "size": size, "level": 1.25,
                      "createdDateUTC": "2022-01-15T14:00:00" },
        "market": { "epic": epic }
    })
}

fn activity(deal: &str, epic: &str, size: &str) -> serde_json::Value {
    json!({
        "dealId": deal, "epic": epic, "type": "POSITION", "status": "ACCEPTED",
        "date": "2022-01-15T14:00:00",
        "details": { "dealReference": format!("ref-{deal}"), "size": size, "level": "1.25",
                     "direction": if size.starts_with('-') { "SELL" } else { "BUY" } }
    })
}

/// With nothing admitted a pass makes exactly the requests it always made - ONE `GET /workingorders` -
/// and the client claims an EMPTY list (a claim: "told nothing").
#[test]
fn nothing_admitted_reads_the_mounted_epic_with_the_old_request() {
    let ig = serve(vec![
        login_route(),
        Route::ok(
            "GET /workingorders",
            json!({ "workingOrders": [working("W1", MOUNTED), working("W2", ADMITTED)] }),
        ),
    ]);
    let client = client(&ig);
    assert_eq!(client.admitted_symbols(), Some(vec![]));
    let reports = client.fetch_order_status_reports(0).expect("orders");
    assert_eq!(reports.len(), 1, "an epic nobody admitted is not reported");
    assert_eq!((reports[0].symbol.as_str(), reports[0].venue_order_id.as_str()), (MOUNTED, "W1"));
    assert_eq!(ig.count("GET /workingorders"), 1);
}

/// An admitted epic's orders come out of the SAME body (one request), mounted first; the mounted epic
/// and an epic told twice (any case) add nothing; the list is what it was told, upper-cased.
#[test]
fn an_admitted_epics_orders_come_out_of_the_one_account_wide_body() {
    let ig = serve(vec![
        login_route(),
        Route::ok(
            "GET /workingorders",
            json!({ "workingOrders": [
                working("W2", ADMITTED), working("W1", MOUNTED), working("W3", FOREIGN),
            ] }),
        ),
    ]);
    let client = client(&ig);
    let lower = ADMITTED.to_ascii_lowercase();
    for told in [ADMITTED, lower.as_str(), MOUNTED] {
        client.admit_symbol(told);
    }
    assert_eq!(client.admitted_symbols(), Some(vec![ADMITTED.to_string()]));
    let reports = client.fetch_order_status_reports(0).expect("orders");
    let seen: Vec<(&str, &str)> =
        reports.iter().map(|r| (r.symbol.as_str(), r.venue_order_id.as_str())).collect();
    assert_eq!(seen, [(MOUNTED, "W1"), (ADMITTED, "W2")], "mounted first, FOREIGN not reported");
    assert_eq!(ig.count("GET /workingorders"), 1, "ONE read however many epics are admitted");
}

/// Positions are aggregated per epic out of one body; an admitted epic IG does not list is a FLAT row (a
/// stale local position stays detectable); a held epic nobody admitted is not reported.
#[test]
fn positions_are_netted_per_epic_and_an_unlisted_admitted_epic_is_flat() {
    let ig = serve(vec![
        login_route(),
        Route::ok(
            "GET /positions",
            json!({ "positions": [
                position("P1", ADMITTED, "BUY", 2.0),
                position("P2", ADMITTED, "SELL", 0.5),
                position("P3", FOREIGN, "BUY", 9.0),
            ] }),
        ),
    ]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    let held = client.fetch_position_status_reports().expect("positions");
    let held: Vec<(&str, f64)> = held.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(
        held,
        [(MOUNTED, 0.0), (ADMITTED, 1.5)],
        "mounted flat, admitted netted, FOREIGN out"
    );
    assert_eq!(ig.count("GET /positions"), 1);
}

/// Fills: one activity read, filtered per epic; the request carries the `from`/`pageSize` it always did.
#[test]
fn fills_come_out_of_one_activity_read_per_epic() {
    let ig = serve(vec![
        login_route(),
        Route::ok(
            "GET /history/activity",
            json!({ "activities": [
                activity("F1", ADMITTED, "+1"),
                activity("F2", MOUNTED, "-50"),
                activity("F3", FOREIGN, "+5"),
            ] }),
        ),
    ]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    let fills = client.fetch_fill_reports(1_642_255_200_000).expect("fills");
    let fills: Vec<(&str, &str, f64)> =
        fills.iter().map(|f| (f.symbol.as_str(), f.trade_id.as_str(), f.last_qty)).collect();
    assert_eq!(fills, [(MOUNTED, "F2", 50.0), (ADMITTED, "F1", 1.0)]);
    assert_eq!(
        ig.count("GET /history/activity?from=2022-01-15T14:00:00&detailed=true&pageSize=50"),
        1
    );
}

/// `(symbol, trade id)` of each fill, in the order reported.
fn fill_keys(fills: &[vike_model::FillReport]) -> Vec<(&str, &str)> {
    fills.iter().map(|f| (f.symbol.as_str(), f.trade_id.as_str())).collect()
}

fn page_reads(ig: &FakeIg) -> usize {
    ig.count("GET /history/activity")
}

/// The double over a paged activity history (`pages[i]` = page `i`'s rows), after the login.
fn paged(pages: Vec<Vec<serde_json::Value>>) -> FakeIg {
    let mut routes = vec![login_route()];
    routes.extend(activity_pages(pages));
    serve(routes)
}

/// The fills read is paged ONLY when an epic is admitted: a pass with nothing admitted reads the one first
/// page, whatever that page says about a next one (the mounted-only behaviour is unchanged).
#[test]
fn a_mounted_only_fills_read_is_one_page_even_when_the_page_names_a_next() {
    let ig = paged(vec![vec![activity("F1", MOUNTED, "+1")], vec![activity("F2", MOUNTED, "+2")]]);
    let fills = client(&ig).fetch_fill_reports(1_642_255_200_000).expect("fills");
    assert_eq!(fill_keys(&fills), [(MOUNTED, "F1")]);
    assert_eq!(page_reads(&ig), 1);
}

/// With an epic admitted the read follows `metadata.paging.next` VERBATIM to the end: an admitted epic's fill
/// on the third page is found; a boundary row two pages both carry is kept once; a foreign epic stays out.
#[test]
fn an_admitted_fills_read_follows_next_to_the_last_page() {
    let boundary = activity("F2", ADMITTED, "+1");
    let ig = paged(vec![
        vec![activity("F1", MOUNTED, "-50"), boundary.clone()],
        vec![boundary, activity("F3", FOREIGN, "+5")],
        vec![activity("F4", ADMITTED, "+3")],
    ]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    let fills = client.fetch_fill_reports(1_642_255_200_000).expect("fills");
    assert_eq!(fill_keys(&fills), [(MOUNTED, "F1"), (ADMITTED, "F2"), (ADMITTED, "F4")]);
    assert_eq!(page_reads(&ig), 3);
    assert_eq!(ig.count(&activity_link(1)), 1, "page two is IG's own link, as written");
    assert_eq!(ig.count(&activity_link(2)), 1);
}

/// The de-duplication is of a repeated ROW, not of a `dealId`: IG may reuse a closed position's deal id on
/// its close, and the single-page read has always kept both rows.
#[test]
fn two_rows_sharing_a_deal_id_are_both_kept_across_pages() {
    let open = activity("F1", ADMITTED, "+1");
    let mut close = activity("F1", ADMITTED, "-1");
    close["date"] = json!("2022-01-15T15:00:00");
    let ig = paged(vec![vec![open], vec![close]]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    let fills = client.fetch_fill_reports(1_642_255_200_000).expect("fills");
    assert_eq!(fills.iter().map(|f| f.side).collect::<Vec<_>>(), [1, -1]);
}

/// The bound is TEN requests: a history that ends on the tenth page is read whole, one that still names a
/// next page after it fails the pass (a short report is never passed off as the whole window).
#[test]
fn a_history_still_truncated_after_ten_pages_fails_the_pass() {
    let history = |pages: usize| {
        let rows = (0..pages).map(|i| vec![activity(&format!("F{i}"), ADMITTED, "+1")]).collect();
        paged(rows)
    };
    let ig = history(10);
    let client_10 = client(&ig);
    client_10.admit_symbol(ADMITTED);
    assert_eq!(client_10.fetch_fill_reports(0).expect("ten pages").len(), 10);
    assert_eq!(page_reads(&ig), 10);

    let ig = history(11);
    let client_11 = client(&ig);
    client_11.admit_symbol(ADMITTED);
    let why = client_11.fetch_fill_reports(0).expect_err("an eleventh page is not read");
    assert!(why.contains("more pages than a pass reads"), "{why}");
    assert_eq!(page_reads(&ig), 10, "no request past the bound");
}

/// A next page that adds no row cannot move the read: the pass fails after it instead of looping to the bound.
#[test]
fn a_next_page_with_no_new_row_fails_the_pass() {
    let row = activity("F1", ADMITTED, "+1");
    let ig = paged(vec![vec![row.clone()], vec![row], vec![]]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    let why = client.fetch_fill_reports(0).expect_err("no progress");
    assert!(why.contains("more pages than a pass reads"), "{why}");
    assert_eq!(page_reads(&ig), 2);
}

/// A `next` that is not a `/history/activity` link is never followed (the session's tokens go only to the
/// gateway it logged in at), and a `next` that is not even a string fails the pass too.
#[test]
fn a_next_link_that_is_not_the_activity_path_fails_the_pass_unfollowed() {
    for next in [
        json!("https://elsewhere.example/history/activity?from=2022-01-15T14:00:00"),
        json!("/positions?version=2"),
        json!(7),
    ] {
        let ig = serve(vec![
            login_route(),
            Route::ok(
                "GET /history/activity",
                json!({ "activities": [activity("F1", ADMITTED, "+1")],
                        "metadata": { "paging": { "next": next } } }),
            ),
        ]);
        let client = client(&ig);
        client.admit_symbol(ADMITTED);
        let why = client.fetch_fill_reports(0).expect_err("an unfollowable next");
        assert!(why.contains("metadata.paging.next"), "{why}");
        let gets = ig.served().iter().filter(|line| line.starts_with("GET")).count();
        assert_eq!(gets, 1, "{next}: nothing was requested beyond the first page");
    }
}

/// A later page IG does not answer fails the WHOLE fetch: the first page's fills are never reported alone.
#[test]
fn an_unanswered_later_page_fails_the_whole_fills_fetch() {
    let ig = serve(vec![
        login_route(),
        Route::ok(
            "GET /history/activity?from=",
            json!({ "activities": [activity("F1", ADMITTED, "+1")],
                    "metadata": { "paging": { "next": activity_link(1) } } }),
        ),
    ]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    assert!(client.fetch_fill_reports(0).is_err());
    assert_eq!(page_reads(&ig), 2);
}

/// A body the client cannot parse fails the WHOLE fetch: a report that lacks an admitted epic's orders
/// would let the restore planner retire them as gone.
#[test]
fn an_unreadable_body_fails_the_whole_order_fetch() {
    let ig = serve(vec![login_route(), Route::ok("GET /workingorders", json!({ "nope": [] }))]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    let why = client.fetch_order_status_reports(0).expect_err("no workingOrders array");
    assert!(why.contains("workingOrders"), "{why}");
}

/// The balance read is untouched by the list (account-wide, no epic in it).
#[test]
fn the_balance_read_is_the_accounts_own() {
    let ig = serve(vec![login_route(), accounts_route("USD")]);
    let client = client(&ig);
    client.admit_symbol(ADMITTED);
    assert_eq!(client.fetch_balance().expect("balance"), Some(1000.0));
}
