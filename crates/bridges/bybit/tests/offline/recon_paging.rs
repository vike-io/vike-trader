//! **The reconcile client reads EVERY page of a symbol's open orders** (offline: a scripted
//! `BybitTransport`, no network, nothing `#[ignore]`d).
//!
//! `order/realtime` lists at most 50 rows a request and hands `nextPageCursor` for the rest. The
//! restore planner reads an order a successful report lacks as gone, so a read that stopped at the
//! first page would call a resting stop gone on a busy symbol. Pins: a short page is the last page
//! (the venue hands a cursor on it too: the recorded demo answer below) and costs one request; full
//! pages are followed by cursor and joined in order; the cursor goes on the wire as the venue spelled
//! it (the signer encodes once, so the client hands it the decoded text); an empty page, an empty
//! cursor and a full last page all end the walk; a REPEATED cursor, a chain past the page cap and a
//! failing page each FAIL the read (never a partial list reported as complete).

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::signer::{BybitV5Signer, urlencode};
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::BybitReconClient;
use vike_bybit::transport::BybitTransport;
use vike_exec::recon::ReconClient;
use vike_model::OrderType;
use vike_secrets::credentials::Credentials;

/// The venue's page size, and the client's ask: a change to either is a change to this suite.
const LIMIT: usize = 50;

/// One request as the transport saw it.
struct Req {
    /// The global request index, from 0.
    n: usize,
    symbol: String,
    cursor: Option<String>,
}

/// Answers each request from a closure and records every param list it was sent.
struct Pages<F: Fn(&Req) -> Result<Value, VenueApiError>> {
    answer: F,
    sent: Mutex<Vec<Vec<(String, Value)>>>,
}

impl<F: Fn(&Req) -> Result<Value, VenueApiError>> BybitTransport for Pages<F> {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        assert!(path.ends_with("/order/realtime"), "an order read only: {path}");
        let params: Vec<(String, Value)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let text = |key: &str| {
            params.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str().map(str::to_string))
        };
        let req = Req {
            n: self.sent.lock().unwrap().len(),
            symbol: text("symbol").unwrap_or_default(),
            cursor: text("cursor"),
        };
        self.sent.lock().unwrap().push(params);
        (self.answer)(&req)
    }
}

fn client<F: Fn(&Req) -> Result<Value, VenueApiError> + Send>(
    answer: F,
) -> BybitReconClient<Pages<F>> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    BybitReconClient::new(
        BybitV5Signer::new(&creds, || 0),
        Pages { answer, sent: Mutex::new(Vec::new()) },
        "https://x",
        "BTCUSDT",
    )
}

fn row(symbol: &str, i: usize) -> Value {
    json!({
        "symbol": symbol, "orderId": format!("o{i}"), "orderLinkId": format!("c{i}"),
        "side": "Buy", "orderType": "Limit", "qty": "0.001", "cumExecQty": "0", "avgPrice": "0",
        "orderStatus": "New", "updatedTime": "1",
    })
}

/// A `{retCode, retMsg, result}` envelope holding rows `rows` of `symbol` and `cursor` as the
/// venue spells it (percent-encoded).
fn page(symbol: &str, rows: std::ops::Range<usize>, cursor: &str) -> Result<Value, VenueApiError> {
    let list: Vec<Value> = rows.map(|i| row(symbol, i)).collect();
    Ok(
        json!({ "retCode": 0, "retMsg": "OK", "result": { "list": list, "nextPageCursor": cursor } }),
    )
}

fn fail(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

fn ids(reports: &[vike_model::OrderStatusReport]) -> Vec<String> {
    reports.iter().map(|r| r.venue_order_id.to_string()).collect()
}

fn requests<F: Fn(&Req) -> Result<Value, VenueApiError>>(
    c: &BybitReconClient<Pages<F>>,
) -> Vec<Vec<(String, Value)>> {
    c.transport.sent.lock().unwrap().clone()
}

/// The row of the batch-6 drill's recorded `order/realtime` answer (Bybit DEMO, linear, 2026-10-24,
/// nothing edited): a reduce-only conditional market stop, and the `nextPageCursor` that answer
/// carried: ONE short page, yet a non-empty cursor.
pub(crate) const RECORDED_STOP_ROW: &str = r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1792811914196","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"80924.3","leavesQty":"0.001","leavesValue":"0","marketUnit":"","orderId":"574d70ee-5014-4a90-afc9-aa47d3e5e42d","orderIv":"","orderLinkId":"b6bybitstop","orderStatus":"Untriggered","orderType":"Market","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"0","qty":"0.001","reduceOnly":true,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"IOC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":2,"triggerPrice":"60693.2","updatedTime":"1792811914196"}"#;
const RECORDED_CURSOR: &str = "574d70ee-5014-4a90-afc9-aa47d3e5e42d%3A1792811914196%2Ca69a4457-883f-4acf-a984-ce758c7f6d54%3A1792811897491";

#[test]
fn a_short_page_is_the_whole_read_even_when_it_carries_a_cursor() {
    let stop: Value = serde_json::from_str(RECORDED_STOP_ROW).unwrap();
    let c = client(|_| {
        Ok(json!({ "retCode": 0, "retMsg": "OK", "result": {
            "category": "linear", "list": [stop.clone()], "nextPageCursor": RECORDED_CURSOR,
        } }))
    });
    let reports = c.fetch_order_status_reports(0).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].order_type,
        OrderType::Stop,
        "the recorded conditional still reads as a stop"
    );
    let sent = requests(&c);
    assert_eq!(sent.len(), 1, "one request: the cursor on a short page is not followed");
    assert_eq!(
        sent[0],
        vec![
            ("category".to_string(), json!("linear")),
            ("symbol".to_string(), json!("BTCUSDT")),
            ("limit".to_string(), json!(LIMIT)),
        ]
    );
}

#[test]
fn an_empty_page_is_an_empty_report_in_one_request() {
    let c = client(|_| page("BTCUSDT", 0..0, ""));
    assert!(c.fetch_order_status_reports(0).unwrap().is_empty());
    assert_eq!(requests(&c).len(), 1);
}

#[test]
fn full_pages_are_followed_by_cursor_and_joined_in_order() {
    const A: &str = "a1%3A10%2Ca2%3A20";
    const B: &str = "b1%3A30%2Cb2%3A40";
    let c = client(|req| match req.cursor.as_deref() {
        None => page("BTCUSDT", 0..LIMIT, A),
        Some("a1:10,a2:20") => page("BTCUSDT", LIMIT..2 * LIMIT, B),
        // the last page is short and still carries a cursor: not followed
        Some("b1:30,b2:40") => page("BTCUSDT", 2 * LIMIT..2 * LIMIT + 3, "z%3A9"),
        other => Err(fail(1, &format!("unexpected cursor {other:?}"))),
    });
    let reports = c.fetch_order_status_reports(0).unwrap();
    let want: Vec<String> = (0..2 * LIMIT + 3).map(|i| format!("o{i}")).collect();
    assert_eq!(ids(&reports), want, "every page, in the venue's order");

    let sent = requests(&c);
    assert_eq!(sent.len(), 3);
    let cursors: Vec<Option<&Value>> =
        sent.iter().map(|p| p.iter().find(|(k, _)| k == "cursor").map(|(_, v)| v)).collect();
    assert_eq!(cursors, [None, Some(&json!("a1:10,a2:20")), Some(&json!("b1:30,b2:40"))]);
    assert!(sent.iter().all(|p| p.contains(&("limit".to_string(), json!(LIMIT)))));
    // The signer encodes a GET value once: the wire must carry the venue's own spelling, not a
    // double-encoded `%253A`.
    for (sent_cursor, venue_spelling) in [("a1:10,a2:20", A), ("b1:30,b2:40", B)] {
        assert_eq!(
            urlencode(&[("cursor", sent_cursor.to_string())]),
            format!("cursor={venue_spelling}")
        );
    }
}

#[test]
fn a_full_last_page_ends_on_an_empty_cursor_and_an_empty_page_ends_on_an_echoed_one() {
    let c = client(|req| match req.cursor {
        None => page("BTCUSDT", 0..LIMIT, ""),
        Some(_) => Err(fail(1, "a page after the empty cursor")),
    });
    assert_eq!(c.fetch_order_status_reports(0).unwrap().len(), LIMIT);
    assert_eq!(requests(&c).len(), 1);

    // The full page's own cursor, then an empty page that echoes it: the empty page ends the walk
    // before the repeat guard looks.
    let c = client(|req| match req.cursor {
        None => page("BTCUSDT", 0..LIMIT, "same"),
        Some(_) => page("BTCUSDT", 0..0, "same"),
    });
    assert_eq!(c.fetch_order_status_reports(0).unwrap().len(), LIMIT);
    assert_eq!(requests(&c).len(), 2);
}

#[test]
fn a_repeated_cursor_fails_the_read_instead_of_spinning() {
    // The same cursor handed back at once.
    let c = client(|_| page("BTCUSDT", 0..LIMIT, "same"));
    let err = c.fetch_order_status_reports(0).unwrap_err();
    assert!(err.contains("repeated a page cursor"), "{err}");
    assert_eq!(requests(&c).len(), 2, "the second page is the one that shows the repeat");

    // A cycle (A, B, A) is caught at its first repeat too.
    let c = client(|req| match req.cursor.as_deref() {
        None => page("BTCUSDT", 0..LIMIT, "A"),
        Some("A") => page("BTCUSDT", 0..LIMIT, "B"),
        Some(_) => page("BTCUSDT", 0..LIMIT, "A"),
    });
    assert!(c.fetch_order_status_reports(0).unwrap_err().contains("repeated a page cursor"));
    assert_eq!(requests(&c).len(), 3);
}

#[test]
fn a_chain_past_the_page_cap_fails_the_read_instead_of_reporting_a_partial_list() {
    let c =
        client(|req| page("BTCUSDT", req.n * LIMIT..(req.n + 1) * LIMIT, &format!("c{}", req.n)));
    let err = c.fetch_order_status_reports(0).unwrap_err();
    assert!(err.contains("still paging after 40 pages"), "{err}");
    assert_eq!(requests(&c).len(), 40, "the cap is 40 pages of 50 rows");
}

#[test]
fn a_failing_page_fails_the_read() {
    let c = client(|req| match req.n {
        0 => page("BTCUSDT", 0..LIMIT, "A"),
        _ => Err(fail(10006, "too many visits")),
    });
    // Never the first page alone: the planner would read the rest as gone.
    assert_eq!(c.fetch_order_status_reports(0).unwrap_err(), "too many visits");
}

#[test]
fn an_admitted_symbol_is_walked_on_its_own_chain() {
    let c = client(|req| match (req.symbol.as_str(), req.cursor.as_deref()) {
        ("BTCUSDT", None) => page("BTCUSDT", 0..2, ""),
        ("ETHUSDT", None) => page("ETHUSDT", 100..100 + LIMIT, "e%3A1"),
        ("ETHUSDT", Some("e:1")) => page("ETHUSDT", 100 + LIMIT..100 + LIMIT + 5, ""),
        other => Err(fail(1, &format!("unexpected request {other:?}"))),
    });
    c.admit_symbol("ETHUSDT");
    let reports = c.fetch_order_status_reports(0).unwrap();
    assert_eq!(reports.len(), 2 + LIMIT + 5);
    assert_eq!(requests(&c).len(), 3, "BTCUSDT once, ETHUSDT twice");

    // …and a chain that never ends on the admitted symbol fails the whole fetch.
    let c = client(|req| match req.symbol.as_str() {
        "BTCUSDT" => page("BTCUSDT", 0..1, ""),
        _ => page("ETHUSDT", 0..LIMIT, "loop"),
    });
    c.admit_symbol("ETHUSDT");
    assert!(c.fetch_order_status_reports(0).is_err());
}
