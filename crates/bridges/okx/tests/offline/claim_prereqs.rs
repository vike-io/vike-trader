//! **The OKX claim prerequisites that can be pinned without a venue** (offline: a scripted
//! `OkxTransport`, no network, nothing `#[ignore]`d). The claim itself, the recon client's
//! `lists_trigger_orders` answer, is NOT made: the crate's own CLAUDE.md lists what still stands
//! between okx and it.
//!
//! The stop read pages. `orders-algo-pending` lists at most 100 rows a request and `after` (an
//! `algoId`) asks for the records earlier than it. The restore planner reads an order a successful
//! report lacks as gone, so a stop past the first page would be read as gone: a short page ends the
//! walk (one request for the common case), a full page is followed by the last row's `algoId`, and a
//! chain that cannot finish (a repeated `algoId`, the page cap, a full page with no cursor, a failing
//! page) FAILS the whole report instead of returning what it has.
//!
//! The proof the conformance harness will ask of okx once the claim is made (a parsed venue row of
//! every trigger kind its capability row lets `submit` place, through the real client, into a LIVE
//! report carrying the coid) is run here already, so the harness row is a copy of this test.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_exec::recon::ReconClient;
use vike_model::OrderType;
use vike_okx::OkxReconClient;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;
use vike_venues::venue_caps::{TRIGGER_KINDS, caps_for};

/// The client's own cap (`ALGO_MAX_PAGES`) and the venue's page size (`ALGO_PAGE_LIMIT`): a change to
/// either is a change to this suite.
const MAX_PAGES: usize = 40;
const LIMIT: usize = 100;

/// One `orders-algo-pending` request as the transport saw it.
struct Req {
    /// The index among the algo requests, from 0.
    n: usize,
    after: Option<String>,
}

type Answer = Box<dyn Fn(&Req) -> Result<Value, VenueApiError> + Send + Sync>;

/// The params of every `orders-algo-pending` request, in order; shared so the test reads them back.
type Sent = Arc<Mutex<Vec<Vec<(String, Value)>>>>;

/// Answers `orders-pending` with no orders and every `orders-algo-pending` request from `algo`,
/// recording the params of each algo request.
struct Venue {
    algo: Answer,
    sent: Sent,
}

impl OkxTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
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
        let mut sent = self.sent.lock().unwrap();
        let req = Req { n: sent.len(), after };
        sent.push(params);
        drop(sent);
        (self.algo)(&req)
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("the recon client must use the SIGNED lane, not public ({path})");
    }
}

fn client(
    algo: impl Fn(&Req) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> (OkxReconClient<Venue>, Sent) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let client = OkxReconClient::new(
        OkxV5Signer::new(&creds, || 0),
        Venue { algo: Box::new(algo), sent: Arc::clone(&sent) },
        "https://unused.invalid",
        "BTC-USDT-SWAP",
        0.01,
    );
    (client, sent)
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
    let coids: Vec<String> =
        reports.iter().map(|r| r.client_order_id.clone().expect("a coid")).collect();
    assert_eq!(coids, (0..207).map(|i| format!("sl{i}")).collect::<Vec<_>>(), "in page order");
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

/// **The proof the harness will ask of okx**: for every trigger kind okx's capability row lets
/// `submit` place, a venue-native `orders-algo-pending` row, through the real client, is a LIVE
/// report of that kind carrying the coid (`crates/vike-bridge-core/tests/bridge_conformance.rs`'s
/// `check_claim`). The kinds are asserted as a list so that a new placeable trigger kind fails here
/// until its own row exists, instead of passing the claim unproven.
#[test]
fn every_trigger_kind_okx_can_place_is_a_live_report_through_the_real_client() {
    let kinds: Vec<OrderType> = caps_for("okx")
        .supported_order_kinds
        .iter()
        .copied()
        .filter(|kind| TRIGGER_KINDS.contains(kind))
        .map(OrderType::from_stored)
        .collect();
    assert_eq!(kinds, [OrderType::Stop], "okx places its stop as one `conditional` algo order");
    for kind in kinds {
        let row = match kind {
            OrderType::Stop => stop_row(0),
            ref other => panic!("no `orders-algo-pending` fixture for the {other:?} okx places"),
        };
        let (c, _) = client(move |_| Ok(envelope(vec![row.clone()])));
        let reports = c.fetch_order_status_reports(0).expect("report");
        let report = reports
            .iter()
            .find(|r| r.client_order_id.as_deref() == Some("sl0"))
            .expect("the row parses to a report carrying the coid");
        assert!(!report.status.is_terminal(), "a listing, not a terminal report");
        assert_eq!(report.order_type, kind, "the planner reads the kind the stop was placed as");
    }
}
