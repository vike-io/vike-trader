//! Pure parse tests for the Alpaca `ReconcileClient` report parsers (ReconFactory seam)
//! — NO network, just representative JSON bodies shaped like the Broker API's real responses (per
//! `alpacahq/alpaca-docs`): `GET /v1/trading/accounts/{id}/orders`, `GET
//! /v1/accounts/activities/FILL`, `GET /v1/trading/accounts/{id}/positions`, `GET
//! /v1/trading/accounts/{id}/account`. Asserts the tricky bits the module doc calls out: the
//! wire-vs-vike symbol relabeling for crypto pairs, unsigned `qty` + `side` -> signed position
//! qty, and the flat-position synthesis when Alpaca omits a symbol entirely.

use vike_alpaca::reconcile::{parse_balance, parse_fills, parse_orders, parse_positions};
use vike_model::events::{LiquiditySide, PositionSide};

// --- parse_orders --------------------------------------------------------------------------

#[test]
fn parses_order_row_every_field_and_normalizes_status() {
    let body = r#"[
        {
            "id": "61e69015-8549-4bfd-b9c3-01e75843f47d",
            "client_order_id": "eb9e2aaf-806f-4de3-a19d-c7a3fca6862d",
            "updated_at": "2026-03-16T18:38:02.000Z",
            "symbol": "AAPL",
            "qty": "5",
            "filled_qty": "2",
            "filled_avg_price": "150.25",
            "order_type": "limit",
            "type": "limit",
            "side": "buy",
            "status": "partially_filled"
        }
    ]"#;
    let r = parse_orders(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "alpaca");
    assert_eq!(o.symbol, "AAPL");
    assert_eq!(o.venue_order_id.as_str(), "61e69015-8549-4bfd-b9c3-01e75843f47d");
    assert_eq!(o.order_client_id.as_deref(), Some("eb9e2aaf-806f-4de3-a19d-c7a3fca6862d"));
    assert_eq!(o.side, 1, "buy -> +1");
    assert_eq!(o.order_type, vike_model::OrderType::Limit);
    assert_eq!(o.qty, 5.0);
    assert_eq!(o.filled_qty, 2.0);
    assert_eq!(o.average_price, 150.25);
    assert_eq!(o.status, vike_model::OrderStatus::PartiallyFilled);
    assert_eq!(o.timestamp, 1773686282000);
}

#[test]
fn order_row_empty_order_client_id_normalizes_to_none() {
    let body = r#"[{"id":"x","client_order_id":"","symbol":"AAPL","side":"sell","status":"new"}]"#;
    let r = parse_orders(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r[0].order_client_id, None);
    assert_eq!(r[0].side, -1, "sell -> -1");
    assert_eq!(r[0].status, vike_model::OrderStatus::Accepted, "new -> ACCEPTED");
}

#[test]
fn parse_orders_relabels_crypto_wire_symbol_to_the_vike_symbol() {
    // Alpaca's wire form slashes bare crypto pairs; the report must carry the UNIFIED vike symbol
    // (what local state is keyed by), not the wire spelling — see the module doc.
    let body = r#"[{"id":"x","symbol":"BTC/USD","side":"buy","qty":"0.5","status":"filled"}]"#;
    let r = parse_orders(body, "BTC/USD", "BTCUSD").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, "BTCUSD", "output carries the vike spelling, not the wire slash");
}

#[test]
fn parse_orders_filters_out_other_symbols() {
    let body = r#"[
        {"id":"a","symbol":"AAPL","side":"buy","status":"new"},
        {"id":"b","symbol":"MSFT","side":"buy","status":"new"}
    ]"#;
    let r = parse_orders(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue_order_id.as_str(), "a");
}

#[test]
fn every_documented_status_normalizes() {
    for (raw, want) in [
        ("new", vike_model::OrderStatus::Accepted),
        ("accepted", vike_model::OrderStatus::Accepted),
        ("pending_new", vike_model::OrderStatus::Accepted),
        ("accepted_for_bidding", vike_model::OrderStatus::Accepted),
        ("pending_replace", vike_model::OrderStatus::Accepted),
        ("replaced", vike_model::OrderStatus::Accepted),
        ("stopped", vike_model::OrderStatus::Accepted),
        ("suspended", vike_model::OrderStatus::Accepted),
        ("calculated", vike_model::OrderStatus::Accepted),
        ("held", vike_model::OrderStatus::Accepted),
        ("partially_filled", vike_model::OrderStatus::PartiallyFilled),
        ("filled", vike_model::OrderStatus::Filled),
        ("canceled", vike_model::OrderStatus::Canceled),
        // Still live until Alpaca says canceled, as on binance and vike-ibkr (owner ruling L3).
        ("pending_cancel", vike_model::OrderStatus::PendingCancel),
        ("done_for_day", vike_model::OrderStatus::Canceled),
        ("expired", vike_model::OrderStatus::Expired),
        ("rejected", vike_model::OrderStatus::Rejected),
    ] {
        let body = format!(r#"[{{"id":"x","symbol":"AAPL","side":"buy","status":"{raw}"}}]"#);
        let r = parse_orders(&body, "AAPL", "AAPL").unwrap();
        assert_eq!(r[0].status, want, "status {raw}");
    }
}

#[test]
fn orders_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_orders("not json", "AAPL", "AAPL").is_err());
    assert!(parse_orders("{}", "AAPL", "AAPL").is_err(), "an object, not an array, is an error");
    assert!(parse_orders("null", "AAPL", "AAPL").is_err());
}

// --- parse_fills ---------------------------------------------------------------------------

#[test]
fn parses_fill_activity_every_field() {
    let body = r#"[
        {
            "id": "20210510100104650::88b5f678-fef5-447b-af15-f21e367e6d8c",
            "account_id": "c8f1ef5d-edc0-4f23-9ee4-378f19cb92a4",
            "activity_type": "FILL",
            "transaction_time": "2021-05-10T14:01:04.650Z",
            "type": "fill",
            "price": "128.33",
            "qty": "1",
            "side": "sell",
            "symbol": "AAPL",
            "order_id": "fe060a1b-5b45-4eba-ba46-c3a3345d8255",
            "order_status": "filled"
        }
    ]"#;
    let r = parse_fills(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "alpaca");
    assert_eq!(f.symbol, "AAPL");
    assert_eq!(
        f.trade_id.as_str(),
        "88b5f678-fef5-447b-af15-f21e367e6d8c",
        "the execution id after the activity id's `::`"
    );
    assert_eq!(f.venue_order_id.as_str(), "fe060a1b-5b45-4eba-ba46-c3a3345d8255");
    assert_eq!(f.order_client_id, None, "activities carry no order-client-id");
    assert_eq!(f.side, -1, "sell -> -1");
    assert_eq!(f.last_qty, 1.0);
    assert_eq!(f.last_price, 128.33);
    assert_eq!(f.commission, 0.0, "no fee field on this activity shape");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.timestamp, 1620655264650);
}

/// Measured on the CI box 2026-11-10: the stream booked the flatten's fill under its `execution_id`, the
/// FILL activity came back as `<transaction time>::<execution_id>`, and the pass raised a `MissingFill`
/// for the fill already booked (confirming it would have sold the share twice). Our own fill now raises
/// nothing, and a fill the daemon never booked is still missing.
#[test]
fn a_fill_the_stream_booked_is_not_missing() {
    let exec_id = "060aa12f-443e-480a-9ee8-76a5dc963940";
    let body = format!(
        r#"[{{"id":"20261110140324323::{exec_id}","activity_type":"FILL","symbol":"AAPL",
            "side":"sell","qty":"1","price":"348.19","order_id":"o1",
            "transaction_time":"2026-11-10T19:03:24.323Z"}}]"#
    );
    let fills = parse_fills(&body, "AAPL", "AAPL").unwrap();
    let local = |seen: &[&str]| vike_execution::recon::ReconcileLocalState {
        venue: "alpaca".into(),
        orders: Default::default(),
        seen_trade_ids: seen.iter().map(|s| s.to_string()).collect(),
        positions: Default::default(),
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let booked = local(&[exec_id]);
    let d = vike_execution::recon::diff(&[], &fills, &[], &booked.as_view(), None);
    assert!(d.is_empty(), "the booked fill is not missing: {d:?}");
    let unbooked = local(&[]);
    let d = vike_execution::recon::diff(&[], &fills, &[], &unbooked.as_view(), None);
    assert_eq!(
        d.iter().map(vike_execution::recon::ReconcileDivergence::kind).collect::<Vec<_>>(),
        [vike_execution::recon::ReconcileDivergenceKind::MissingFill]
    );
}

#[test]
fn parse_fills_ignores_non_fill_activity_types() {
    let body = r#"[
        {"id":"a","activity_type":"CSD","symbol":"AAPL"},
        {"id":"b","activity_type":"FILL","symbol":"AAPL","side":"buy","qty":"1","price":"1.0","order_id":"o1"}
    ]"#;
    let r = parse_fills(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "b");
}

#[test]
fn parse_fills_filters_out_other_symbols() {
    let body = r#"[
        {"id":"a","activity_type":"FILL","symbol":"AAPL","side":"buy"},
        {"id":"b","activity_type":"FILL","symbol":"MSFT","side":"buy"}
    ]"#;
    let r = parse_fills(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "a");
}

#[test]
fn fills_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_fills("not json", "AAPL", "AAPL").is_err());
    assert!(parse_fills("{}", "AAPL", "AAPL").is_err(), "an object, not an array, is an error");
}

// --- parse_positions -----------------------------------------------------------------------

#[test]
fn parses_long_position_row() {
    let body = r#"[
        {
            "asset_id": "904837e3-3b76-47ec-b432-046db621571b",
            "symbol": "AAPL",
            "avg_entry_price": "100.0",
            "qty": "5",
            "side": "long"
        }
    ]"#;
    let r = parse_positions(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    let p = &r[0];
    assert_eq!(p.symbol, "AAPL");
    assert_eq!(p.position_side, PositionSide::Both, "netted: the sign is in qty");
    assert_eq!(p.qty, 5.0);
    assert_eq!(p.average_price, 100.0);
}

#[test]
fn parses_short_position_row_as_negative_qty() {
    let body = r#"[{"symbol":"AAPL","avg_entry_price":"50.0","qty":"3","side":"short"}]"#;
    let r = parse_positions(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r[0].position_side, PositionSide::Both, "netted: the sign is in qty");
    assert_eq!(r[0].qty, -3.0, "unsigned wire qty + side=short -> negative");
}

/// Measured on the CI box 2026-11-10: the book held AAPL 1 under `Both` (every alpaca fill folds there) and
/// the venue's row came back `Long`, so every pass raised an `OrphanLocalPosition` for the book's row AND
/// a `PositionOnlyExternal` for the venue's, one share read twice. The same share on both sides now
/// raises nothing, and a real difference is still a drift.
#[test]
fn the_venue_row_meets_the_books_both_position() {
    let local = |qty: f64| vike_execution::recon::ReconcileLocalState {
        venue: "alpaca".into(),
        orders: Default::default(),
        seen_trade_ids: Default::default(),
        positions: [(("AAPL".to_string(), PositionSide::Both), qty)].into_iter().collect(),
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    for (side, qty) in [("long", 1.0), ("short", -1.0)] {
        let body = format!(
            r#"[{{"symbol":"AAPL","avg_entry_price":"313.97","qty":"1","side":"{side}"}}]"#
        );
        let positions = parse_positions(&body, "AAPL", "AAPL").unwrap();
        let held = local(qty);
        let d = vike_execution::recon::diff(&[], &[], &positions, &held.as_view(), None);
        assert!(d.is_empty(), "{side}: one share, read once: {d:?}");
        let held = local(2.0 * qty);
        let d = vike_execution::recon::diff(&[], &[], &positions, &held.as_view(), None);
        assert_eq!(
            d.iter().map(vike_execution::recon::ReconcileDivergence::kind).collect::<Vec<_>>(),
            [vike_execution::recon::ReconcileDivergenceKind::PositionDrift],
            "{side}: a real difference is one drift, not an orphan plus an external"
        );
    }
}

#[test]
fn absent_symbol_synthesizes_a_flat_row() {
    // Alpaca omits a symbol entirely once flat — no row at all, unlike a `qty: "0"` row.
    let body = r#"[{"symbol":"MSFT","avg_entry_price":"1.0","qty":"1","side":"long"}]"#;
    let r = parse_positions(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, "AAPL");
    assert_eq!(r[0].position_side, PositionSide::Both);
    assert_eq!(r[0].qty, 0.0);
}

#[test]
fn empty_positions_array_synthesizes_a_flat_row() {
    let r = parse_positions("[]", "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].qty, 0.0);
}

#[test]
fn parse_positions_relabels_crypto_wire_symbol_to_the_vike_symbol() {
    let body = r#"[{"symbol":"BTC/USD","avg_entry_price":"20000.0","qty":"0.1","side":"long"}]"#;
    let r = parse_positions(body, "BTC/USD", "BTCUSD").unwrap();
    assert_eq!(r[0].symbol, "BTCUSD");
}

#[test]
fn positions_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_positions("not json", "AAPL", "AAPL").is_err());
    assert!(parse_positions("{}", "AAPL", "AAPL").is_err(), "an object, not an array, is an error");
}

// --- parse_balance ---------------------------------------------------------------------------

#[test]
fn parses_account_cash() {
    let body = r#"{"cash":"-23140.2","equity":"103820.56","buying_power":"262113.632"}"#;
    assert_eq!(parse_balance(body).unwrap(), Some(-23140.2));
}

#[test]
fn balance_missing_cash_field_is_none() {
    assert_eq!(parse_balance(r#"{"equity":"100.0"}"#).unwrap(), None);
}

#[test]
fn balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_balance("not json").is_err());
    assert!(parse_balance("[]").is_err(), "an array, not an object, is an error");
    assert!(parse_balance("null").is_err());
}

// --- the `recon_client` factory — no network, just proves the wiring is infallible ------------

#[test]
fn reconcile_client_factory_builds_for_any_configured_symbol() {
    let config = vike_alpaca::AlpacaConfig {
        client_id: vike_model::secret::Secret::new("cid".to_string()),
        client_secret: vike_model::secret::Secret::new("csecret".to_string()),
        account_id: "acct-1".to_string(),
        env: vike_model::AccountTier::Demo,
        hosts: vike_alpaca::urls_for(
            vike_model::AccountTier::Demo,
            &vike_venues::venue_urls::VenueUrls::seed("alpaca"),
        )
        .expect("the seed's sandbox rows"),
    };
    assert!(
        vike_alpaca::recon_client(&config, "AAPL").is_some(),
        "construction is pure/infallible (no network) — always Some"
    );
}

// --- the pagers (`read_order_pages`, `read_fill_pages`) — a scripted page fetch, no network -------

use vike_alpaca::reconcile::{
    FILL_PAGE_CAP, FILL_PAGE_SIZE, ORDER_PAGE_CAP, ORDER_PAGE_LIMIT, read_fill_pages,
    read_order_pages,
};

/// `key=value` as the query encoder spells it on the wire.
fn pair(key: &str, value: &str) -> String {
    vike_bridge_core::urlencode(&[(key, value.to_string())])
}

/// Whether `query` carries the whole pair `key=value` (never a prefix of another pair).
fn has(query: &str, key: &str, value: &str) -> bool {
    query.split('&').any(|p| p == pair(key, value))
}

fn json(rows: &[serde_json::Value]) -> String {
    serde_json::Value::Array(rows.to_vec()).to_string()
}

/// A page fetch that answers `pages` in turn (`[]` once they run out) and records every query.
fn scripted(
    pages: Vec<String>,
    queries: &mut Vec<String>,
) -> impl FnMut(&str) -> Result<String, String> + '_ {
    let mut pages = pages.into_iter();
    move |q| {
        queries.push(q.to_string());
        Ok(pages.next().unwrap_or_else(|| "[]".to_string()))
    }
}

/// An order row of the documented shape (`submitted_at` to the microsecond, as the docs' samples).
fn order(id: &str, submitted_at: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "client_order_id": "", "symbol": "AAPL", "side": "buy", "qty": "1",
        "filled_qty": "0", "order_type": "limit", "type": "limit", "status": status,
        "submitted_at": submitted_at, "updated_at": submitted_at
    })
}

/// `n` terminal orders submitted on 2026-03-`day`, newest first (the `direction=desc` order).
fn order_page(day: u32, n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            let at = format!("2026-03-{day:02}T12:00:00.{:06}Z", 999_999 - i);
            order(&format!("d{day}-{i}"), &at, "filled")
        })
        .collect()
}

fn oldest(page: &[serde_json::Value]) -> String {
    page.last().unwrap()["submitted_at"].as_str().unwrap().to_string()
}

/// The old read was ONE request with no `limit`: the venue's default page (50 rows), so an OPEN
/// order older than it was missing and read as gone. Now every page is read, older and older.
#[test]
fn the_order_read_pages_older_to_a_short_page_and_keeps_an_open_order_from_page_two() {
    let page1 = order_page(16, ORDER_PAGE_LIMIT);
    let mut page2 = order_page(15, ORDER_PAGE_LIMIT);
    page2[0] = page1[ORDER_PAGE_LIMIT - 1].clone(); // the boundary row, served a second time
    page2[1] = order("open-old", "2026-03-15T12:00:00.999998Z", "new");
    let page3 = order_page(14, 3);
    let mut queries = Vec::new();
    let pages = vec![json(&page1), json(&page2), json(&page3)];
    let body = read_order_pages("AAPL,MSFT", scripted(pages, &mut queries)).unwrap();

    let reports = parse_orders(&body, "AAPL", "AAPL").unwrap();
    let ids: Vec<&str> = reports.iter().map(|o| o.venue_order_id.as_str()).collect();
    let want: Vec<&str> =
        page1.iter().chain(&page2[1..]).chain(&page3).map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, want, "every page, in order, the repeated boundary row once");
    assert_eq!(ids.len(), 2 * ORDER_PAGE_LIMIT - 1 + 3);
    let open = reports.iter().find(|o| o.venue_order_id.as_str() == "open-old");
    assert_eq!(
        open.expect("the OPEN order only page 2 carries").status,
        vike_model::OrderStatus::Accepted
    );

    assert_eq!(queries.len(), 3, "a page shorter than the limit ends the read");
    for q in &queries {
        assert!(has(q, "status", "all"), "terminal history is still asked for: {q}");
        assert!(has(q, "symbols", "AAPL,MSFT"), "{q}");
        assert!(has(q, "limit", &ORDER_PAGE_LIMIT.to_string()), "{q}");
        assert!(has(q, "direction", "desc"), "{q}");
    }
    assert!(!queries[0].contains("until="), "the first page is the newest: {}", queries[0]);
    assert!(has(&queries[1], "until", &oldest(&page1)), "{}", queries[1]);
    assert!(has(&queries[2], "until", &oldest(&page2)), "{}", queries[2]);
}

/// The old read returned whatever one page held as if it were everything.
#[test]
fn an_order_read_that_never_reaches_a_short_page_is_an_err_naming_the_cap() {
    let full = json(&order_page(16, ORDER_PAGE_LIMIT));
    let mut calls = 0;
    let r = read_order_pages("AAPL", |_| {
        calls += 1;
        Ok(full.clone())
    });
    let e = r.expect_err("a short list must never read as complete");
    assert!(e.contains(&ORDER_PAGE_CAP.to_string()) && e.contains("cap"), "{e}");
    assert_eq!(calls, ORDER_PAGE_CAP);
}

#[test]
fn an_order_page_that_fails_or_cannot_page_on_fails_the_read() {
    let full = json(&order_page(16, ORDER_PAGE_LIMIT));
    let r = read_order_pages("AAPL", |q| {
        if q.contains("until=") { Err("HTTP 500".to_string()) } else { Ok(full.clone()) }
    });
    assert_eq!(r, Err("HTTP 500".to_string()), "the page fetch's error, unchanged");

    let mut no_stamp = order_page(16, ORDER_PAGE_LIMIT);
    no_stamp[ORDER_PAGE_LIMIT - 1].as_object_mut().unwrap().remove("submitted_at");
    let no_stamp = json(&no_stamp);
    let r = read_order_pages("AAPL", |_| Ok(no_stamp.clone()));
    assert!(r.expect_err("no cursor on a full page").contains("submitted_at"));
}

/// A FILL activity of the documented shape.
fn fill(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "activity_type": "FILL", "transaction_time": "2026-03-16T12:00:00.500Z",
        "type": "fill", "price": "1.0", "qty": "1", "side": "buy", "symbol": "AAPL",
        "order_id": "o1"
    })
}

fn fill_page(tag: &str, n: usize) -> Vec<serde_json::Value> {
    (0..n).map(|i| fill(&format!("{tag}-{i}"))).collect()
}

/// The old read sent `account_id` alone: no window, no paging, whatever the venue's default page held.
#[test]
fn the_fill_read_asks_from_since_and_pages_on_the_last_id() {
    let since = 1_773_662_400_123; // 2026-03-16T12:00:00.123Z
    let (page1, page2) = (fill_page("a", FILL_PAGE_SIZE), fill_page("b", 2));
    let mut queries = Vec::new();
    let pages = vec![json(&page1), json(&page2)];
    let body = read_fill_pages("acct-1", since, scripted(pages, &mut queries)).unwrap();

    let reports = parse_fills(&body, "AAPL", "AAPL").unwrap();
    let ids: Vec<&str> = reports.iter().map(|f| f.trade_id.as_str()).collect();
    let want: Vec<&str> = page1.iter().chain(&page2).map(|f| f["id"].as_str().unwrap()).collect();
    assert_eq!(ids, want, "both pages, oldest first");

    assert_eq!(queries.len(), 2, "a page shorter than the page size ends the read");
    for q in &queries {
        assert!(has(q, "account_id", "acct-1"), "{q}");
        assert!(has(q, "direction", "asc"), "{q}");
        assert!(has(q, "page_size", &FILL_PAGE_SIZE.to_string()), "{q}");
        assert!(has(q, "after", "2026-03-16T12:00:00Z"), "since, floored to its second: {q}");
    }
    assert!(!queries[0].contains("page_token="), "{}", queries[0]);
    let last = format!("a-{}", FILL_PAGE_SIZE - 1);
    assert!(has(&queries[1], "page_token", &last), "{}", queries[1]);
}

#[test]
fn a_fill_read_without_since_asks_from_the_start() {
    for since in [0, -1] {
        let mut queries = Vec::new();
        read_fill_pages("acct-1", since, scripted(vec![json(&fill_page("a", 1))], &mut queries))
            .unwrap();
        assert_eq!(queries.len(), 1);
        assert!(!queries[0].contains("after="), "since {since}: {}", queries[0]);
    }
}

#[test]
fn a_fill_read_that_never_reaches_a_short_page_is_an_err_naming_the_cap() {
    let full = json(&fill_page("a", FILL_PAGE_SIZE));
    let mut calls = 0;
    let r = read_fill_pages("acct-1", 1, |_| {
        calls += 1;
        Ok(full.clone())
    });
    let e = r.expect_err("a short list must never read as complete");
    assert!(e.contains(&FILL_PAGE_CAP.to_string()) && e.contains("cap"), "{e}");
    assert_eq!(calls, FILL_PAGE_CAP);
}

// --- the order report's terms, by kind (the documented order object's fields) --------------------

/// One documented order row carrying `fields` (every number a JSON string or `null`, as the wire).
fn order_row(fields: &str) -> String {
    format!(r#"[{{"id":"x","symbol":"AAPL","side":"sell","qty":"1","status":"new",{fields}}}]"#)
}

/// The report's `(price, trigger_price, trail, trail_percent)`.
fn terms(body: &str) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    let o = &parse_orders(body, "AAPL", "AAPL").unwrap()[0];
    (o.price, o.trigger_price, o.trail, o.trail_percent)
}

#[test]
fn a_limit_row_states_its_limit_price() {
    let body = order_row(
        r#""order_type":"limit","type":"limit","limit_price":"150.25","stop_price":null,
           "trail_price":null,"trail_percent":null,"hwm":null"#,
    );
    assert_eq!(terms(&body), (Some(150.25), None, None, None));
}

#[test]
fn a_stop_row_states_its_stop_price_as_the_trigger() {
    let body = order_row(
        r#""order_type":"stop","type":"stop","limit_price":null,"stop_price":"140.5",
           "trail_price":null,"trail_percent":null,"hwm":null"#,
    );
    assert_eq!(terms(&body), (None, Some(140.5), None, None));
}

/// `type` alone (`order_type` is documented deprecated) picks the terms as it picks the kind.
#[test]
fn a_stop_limit_row_states_both_prices() {
    let body = order_row(
        r#""type":"stop_limit","limit_price":"139.9","stop_price":"140.5",
           "trail_price":null,"trail_percent":null,"hwm":null"#,
    );
    assert_eq!(terms(&body), (Some(139.9), Some(140.5), None, None));
}

/// The venue's `stop_price` on a trailing stop is its MOVING stop (`hwm` - trail), never the report's
/// trigger price (an activation price, which Alpaca has none of).
#[test]
fn a_trailing_stop_by_price_states_its_dollar_trail_and_no_trigger() {
    let body = order_row(
        r#""order_type":"trailing_stop","type":"trailing_stop","limit_price":null,
           "stop_price":"148.00","trail_price":"2.00","trail_percent":null,"hwm":"150.00""#,
    );
    assert_eq!(terms(&body), (None, None, Some(2.0), None));
}

/// Alpaca's `trail_percent` `1.0` is 1% (a sell's stop is `hwm * 0.99`), the unit vike's field takes.
#[test]
fn a_trailing_stop_by_percent_states_its_percent_unconverted_and_no_trigger() {
    let body = order_row(
        r#""order_type":"trailing_stop","type":"trailing_stop","limit_price":null,
           "stop_price":"148.50","trail_price":null,"trail_percent":"1.0","hwm":"150.00""#,
    );
    assert_eq!(terms(&body), (None, None, None, Some(1.0)));
}

#[test]
fn a_market_row_states_no_terms() {
    let body = order_row(
        r#""order_type":"market","type":"market","notional":"500","limit_price":null,
           "stop_price":null,"trail_price":null,"trail_percent":null,"hwm":null"#,
    );
    assert_eq!(terms(&body), (None, None, None, None));
}

/// A term the row does not state is `None`, never `0.0`: absent, `null`, zero or unparseable.
#[test]
fn a_limit_row_without_a_usable_limit_price_states_none() {
    for limit in ["", r#","limit_price":null"#, r#","limit_price":"0""#, r#","limit_price":"abc""#]
    {
        let body = order_row(&format!(r#""order_type":"limit","type":"limit"{limit}"#));
        assert_eq!(terms(&body), (None, None, None, None), "limit_price {limit:?}");
    }
}
