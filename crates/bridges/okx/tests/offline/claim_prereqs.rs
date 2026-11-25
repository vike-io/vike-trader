//! **The OKX claim prerequisites that can be pinned without a venue** (offline: a scripted
//! `RestTransportJson`, no network, nothing `#[ignore]`d). The claim itself, the recon client's
//! `lists_trigger_orders` answer, is made: the crate's own CLAUDE.md says what it rests on, and
//! `claim_restart.rs` runs the restart rules it enables.
//!
//! The stop read pages. `orders-algo-pending` lists at most 100 rows a request and `after` (an
//! `algoId`) asks for the records earlier than it. The restore planner reads an order a successful
//! report lacks as gone, so a stop past the first page would be read as gone: a short page ends the
//! walk (one request for the common case), a full page is followed by the last row's `algoId`, and a
//! chain that cannot finish (a repeated `algoId`, the page cap, a full page with no cursor, a failing
//! page) FAILS the whole report instead of returning what it has.
//!
//! The proof the conformance harness asks of okx for its claim (a parsed venue row of every trigger
//! kind its capability row lets `submit` place, through the real client, into a LIVE report carrying
//! the order client id) is run here too, so the harness row is a copy of this test.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::ReconcileClient;
use vike_model::OrderType;
use vike_okx::ReconcileClientOkx;
use vike_okx::signer::OkxV5Signer;
use vike_secrets::credentials::Credentials;
use vike_venues::venue_caps::capabilities_for;

/// The client's own cap (`ALGO_MAX_PAGES`) and the venue's page size (`ALGO_PAGE_LIMIT`): a change to
/// either is a change to this suite.
const MAX_PAGES: usize = 40;
const LIMIT: usize = 100;

/// One `orders-algo-pending` request as the transport saw it.
struct SeenRequest {
    /// The index among the algo requests, from 0.
    n: usize,
    after: Option<String>,
}

type Answer = Box<dyn Fn(&SeenRequest) -> Result<Value, VenueApiError> + Send + Sync>;

/// The params of every `orders-algo-pending` request, in order; shared so the test reads them back.
type Sent = Arc<Mutex<Vec<Vec<(String, Value)>>>>;

/// Answers `orders-pending` with no orders and every `orders-algo-pending` request of the STOPS
/// (`ordType=conditional`) from `algo`, recording the params of each; the account-wide TRAILING read
/// (`ordType=move_order_stop`, `trailing.rs`'s suite pins its shape) is answered from `trailing` and
/// recorded apart, so the stop walk's own count stays the one these tests read.
struct Venue {
    algo: Answer,
    sent: Sent,
    trailing: Answer,
    trailing_sent: Sent,
}

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        if path.ends_with("/orders-pending") {
            return Ok(envelope(Vec::new()));
        }
        assert!(path.ends_with("/orders-algo-pending"), "an order read only: {path}");
        let params: Vec<(String, Value)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let after = params
            .iter()
            .find(|(k, _)| k == "after")
            .and_then(|(_, v)| v.as_str().map(String::from));
        let trailing = value(&params, "ordType") == Some(&json!("move_order_stop"));
        let (answer, sent) =
            if trailing { (&self.trailing, &self.trailing_sent) } else { (&self.algo, &self.sent) };
        let mut sent = sent.lock().unwrap();
        let req = SeenRequest { n: sent.len(), after };
        sent.push(params);
        drop(sent);
        answer(&req)
    }

    vike_bridge_core::no_public_call!("the recon client must use the SIGNED lane, not public");
}

/// The client with okx's row CLOSED for `trailing` (stated, so a flip of the row moves none of these pins):
/// the stop walk alone, as before the trailing order existed.
fn client(
    algo: impl Fn(&SeenRequest) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> (ReconcileClientOkx<Venue>, Sent) {
    let (client, sent, _) = client_with(algo, |_| page(0..0));
    (client.with_trailing(false), sent)
}

/// The client with okx's row taken as LISTING `trailing`: its account-wide trailing read answers from
/// `trailing`; the third value records it.
fn client_with(
    algo: impl Fn(&SeenRequest) -> Result<Value, VenueApiError> + Send + Sync + 'static,
    trailing: impl Fn(&SeenRequest) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> (ReconcileClientOkx<Venue>, Sent, Sent) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let trailing_sent = Arc::new(Mutex::new(Vec::new()));
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let client = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, || 0),
        Venue {
            algo: Box::new(algo),
            sent: Arc::clone(&sent),
            trailing: Box::new(trailing),
            trailing_sent: Arc::clone(&trailing_sent),
        },
        "https://unused.invalid",
        "BTC-USDT-SWAP",
        0.01,
    )
    .with_trailing(true);
    (client, sent, trailing_sent)
}

fn envelope(rows: Vec<Value>) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

/// The id of the i-th stop: descending, as a newest-first list would hand them.
fn algo_id(i: usize) -> String {
    (900_000 - i).to_string()
}

fn stop_row(i: usize) -> Value {
    json!({
        "instId": "BTC-USDT-SWAP", "algoId": algo_id(i), "algoClOrdId": format!("sl{i}"),
        "ordType": "conditional", "side": "sell", "sz": "1", "state": "live",
        "slTriggerPx": "50000", "reduceOnly": "true", "uTime": "1700000000000"
    })
}

fn page(rows: std::ops::Range<usize>) -> Result<Value, VenueApiError> {
    Ok(envelope(rows.map(stop_row).collect()))
}

fn value<'a>(params: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

#[test]
fn a_short_page_is_the_whole_read_and_costs_one_request() {
    let (c, sent) = client(|_| page(0..3));
    let reports = c.fetch_order_status_reports(0).expect("report");
    assert_eq!(reports.len(), 3);
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "a short page ends the walk");
    assert_eq!(value(&sent[0], "ordType"), Some(&json!("conditional")));
    assert_eq!(value(&sent[0], "instId"), Some(&json!("BTC-USDT-SWAP")));
    assert_eq!(
        value(&sent[0], "limit"),
        Some(&json!("100")),
        "the venue's maximum, asked outright"
    );
    assert_eq!(value(&sent[0], "after"), None, "the first page has no cursor");
}

#[test]
fn full_pages_are_followed_by_the_last_algo_id_and_joined_in_order() {
    let (c, sent) = client(|req| match req.after.as_deref() {
        None => page(0..100),
        Some(a) if a == algo_id(99) => page(100..200),
        Some(a) if a == algo_id(199) => page(200..207),
        other => panic!("request {} asked after {other:?}", req.n),
    });
    let reports = c.fetch_order_status_reports(0).expect("report");
    assert_eq!(reports.len(), 207, "a stop past the first page is not invisible");
    let order_client_ids: Vec<String> =
        reports.iter().map(|r| r.order_client_id.clone().expect("an order client id")).collect();
    assert_eq!(
        order_client_ids,
        (0..207).map(|i| format!("sl{i}")).collect::<Vec<_>>(),
        "in page order"
    );
    assert_eq!(sent.lock().unwrap().len(), 3);
}

#[test]
fn a_full_last_page_ends_on_the_empty_page_after_it() {
    let (c, sent) = client(|req| match req.n {
        0 => page(0..100),
        _ => page(0..0),
    });
    assert_eq!(c.fetch_order_status_reports(0).expect("report").len(), 100);
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(value(&sent[1], "after"), Some(&json!(algo_id(99))));
}

/// A venue that ignores `after` (or sorts the other way) hands the same rows again: that is no
/// "complete" list, and the read fails at the second page.
#[test]
fn a_repeated_algo_id_fails_the_whole_report() {
    let (c, sent) = client(|_| page(0..100));
    let why = c.fetch_order_status_reports(0).expect_err("a chain that repeats is refused");
    assert!(why.contains("orders-algo-pending"), "the error names the stop read: {why}");
    assert!(why.contains("second page"), "and the repeat: {why}");
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[test]
fn a_chain_past_the_page_cap_fails_the_whole_report() {
    let (c, sent) = client(|req| page(req.n * LIMIT..(req.n + 1) * LIMIT));
    let why = c.fetch_order_status_reports(0).expect_err("a chain with no end is refused");
    assert!(why.contains("orders-algo-pending"), "{why}");
    assert!(why.contains(&format!("{MAX_PAGES} pages")), "{why}");
    assert_eq!(sent.lock().unwrap().len(), MAX_PAGES, "and stops asking at the cap");
}

#[test]
fn a_failing_page_fails_the_whole_report_not_a_partial_one() {
    for failure in [
        Ok(json!({"code": "50011", "msg": "rate limit", "data": []})),
        Err(VenueApiError { code: 0, msg: "timed out".into() }),
    ] {
        let (c, _) = client(move |req| match req.n {
            0 => page(0..100),
            _ => failure.clone(),
        });
        let why = c.fetch_order_status_reports(0).expect_err("page 2 failed: no report");
        assert!(why.contains("orders-algo-pending"), "{why}");
    }
}

#[test]
fn a_full_page_whose_last_row_has_no_algo_id_fails_the_whole_report() {
    let (c, sent) = client(|_| {
        let mut rows: Vec<Value> = (0..LIMIT).map(stop_row).collect();
        rows[LIMIT - 1]["algoId"] = json!("");
        Ok(envelope(rows))
    });
    let why = c.fetch_order_status_reports(0).expect_err("no cursor to follow");
    assert!(why.contains("no algoId"), "{why}");
    assert_eq!(sent.lock().unwrap().len(), 1, "nothing is asked after a cursor that is not there");
}

/// The account-wide TRAILING read (okx's row listing `trailing`) is the same walk (`resting_algo_orders`):
/// asked with `ordType` and no `instId`, a full page followed by its last `algoId`. A chain that cannot
/// finish is refused as the stop walk refuses it, but, unmeasured, it leaves out the trailing rows alone
/// (logged) and never fails the report: the stops are still there.
#[test]
fn the_trailing_read_pages_as_the_stop_read_does() {
    let trailing_page = |rows: std::ops::Range<usize>| -> Result<Value, VenueApiError> {
        let rows = rows
            .map(|i| {
                let mut row = stop_row(i);
                row["ordType"] = json!("move_order_stop");
                row["slTriggerPx"] = json!("");
                row
            })
            .collect();
        Ok(envelope(rows))
    };
    let (c, sent, trailing) = client_with(
        |_| page(0..0),
        move |req: &SeenRequest| match req.after.as_deref() {
            None => trailing_page(0..100),
            Some(a) if a == algo_id(99) => trailing_page(100..103),
            other => panic!("trailing request {} asked after {other:?}", req.n),
        },
    );
    let reports = c.fetch_order_status_reports(0).expect("report");
    assert_eq!(reports.len(), 103, "a trailing order past the first page is not invisible");
    assert!(reports.iter().all(|r| r.order_type == OrderType::Trailing));
    assert_eq!(sent.lock().unwrap().len(), 1, "the stop walk is untouched");
    let trailing = trailing.lock().unwrap();
    assert_eq!(trailing.len(), 2);
    assert_eq!(value(&trailing[0], "instId"), None, "account-wide: no instrument named");
    assert_eq!(value(&trailing[0], "limit"), Some(&json!("100")));
    assert_eq!(value(&trailing[1], "after"), Some(&json!(algo_id(99))));
    drop(trailing);

    let (c, _, trailing) = client_with(|_| page(0..3), move |_| trailing_page(0..100));
    let reports =
        c.fetch_order_status_reports(0).expect("a repeating trailing chain blinds nothing");
    assert_eq!(reports.len(), 3, "the stops alone: no trailing row of a chain that repeats");
    assert!(reports.iter().all(|r| r.order_type == OrderType::Stop), "{reports:?}");
    assert_eq!(trailing.lock().unwrap().len(), 2, "refused at the repeat, as the stop walk is");
}

/// **The proof the harness asks of okx**: for every trigger kind okx's capability row lets
/// `submit` place, a venue-native `orders-algo-pending` row, through the real client, is a LIVE
/// report of that kind carrying the order client id (`crates/vike-bridge-core/tests/bridge_conformance.rs`'s
/// `check_claim`). The kinds are asserted as a list so that a new placeable trigger kind fails here
/// until its own row exists, instead of passing the claim unproven.
#[test]
fn every_trigger_kind_okx_can_place_is_a_live_report_through_the_real_client() {
    let kinds: Vec<OrderType> = capabilities_for("okx")
        .supported_order_kinds
        .iter()
        .copied()
        .map(OrderType::from_stored)
        .filter(OrderType::is_trigger)
        .collect();
    assert_eq!(
        kinds,
        [OrderType::Stop, OrderType::StopLimit, OrderType::TakeProfit, OrderType::Trailing],
        "okx places its stop, its stop-limit and its take-profit as one `conditional` algo order each, \
         and its trailing stop as a `move_order_stop` one"
    );
    for kind in kinds {
        let row = match kind {
            OrderType::Stop => stop_row(0),
            // the stop's row with the limit price in `slOrdPx` (`-1` is the stop's market)
            OrderType::StopLimit => {
                let mut row = stop_row(0);
                row["slOrdPx"] = json!("49900");
                row
            }
            // the same `conditional` on its take-profit side: `tpTriggerPx`, `tpOrdPx` `-1` (market-if-touched)
            OrderType::TakeProfit => {
                let mut row = stop_row(0);
                row["slTriggerPx"] = json!("");
                row["tpTriggerPx"] = json!("60000");
                row["tpOrdPx"] = json!("-1");
                row
            }
            // a `move_order_stop` row, listed by the account-wide trailing read alone (`trailing.rs`)
            OrderType::Trailing => {
                let mut row = stop_row(0);
                row["ordType"] = json!("move_order_stop");
                row["slTriggerPx"] = json!("");
                row["callbackSpread"] = json!("150");
                let (c, _, _) =
                    client_with(|_| page(0..0), move |_| Ok(envelope(vec![row.clone()])));
                let reports = c.fetch_order_status_reports(0).expect("report");
                let report = reports
                    .iter()
                    .find(|r| r.order_client_id.as_deref() == Some("sl0"))
                    .expect("the trailing row parses to a report carrying the order client id");
                assert!(!report.status.is_terminal(), "a listing, not a terminal report");
                assert_eq!(report.order_type, kind, "the planner reads the kind it was placed as");
                continue;
            }
            ref other => panic!("no `orders-algo-pending` fixture for the {other:?} okx places"),
        };
        let (c, _) = client(move |_| Ok(envelope(vec![row.clone()])));
        let reports = c.fetch_order_status_reports(0).expect("report");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some("sl0"))
            .expect("the row parses to a report carrying the order client id");
        assert!(!report.status.is_terminal(), "a listing, not a terminal report");
        assert_eq!(report.order_type, kind, "the planner reads the kind the stop was placed as");
    }
}
