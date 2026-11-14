//! **The reconcile read of an instrument's plain resting orders walks every page** (owner ruling F-1;
//! offline: a scripted `RestTransportJson`). `orders-pending` lists at most 100 rows a request and `after` (an
//! `ordId`) asks for the records earlier than it. The read used to be ONE page with no `limit`, so the
//! 101st order on an instrument was missing from a report the restore planner trusts as complete (it reads
//! an order a successful report lacks as gone). It now follows the stop read's rule
//! (`claim_prereqs.rs` pins that walk): a short page ends it, a full page is followed by its last `ordId`,
//! and a chain that cannot finish fails the report.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::ReconcileClient;
use vike_okx::ReconcileClientOkx;
use vike_okx::signer::OkxV5Signer;
use vike_secrets::credentials::Credentials;

/// The params of every `orders-pending` request, in order.
type Sent = Arc<Mutex<Vec<Vec<(String, Value)>>>>;

/// One page's answer by the request's index and the `after` it was asked with.
type Plain = Box<dyn Fn(usize, Option<&str>) -> Value + Send + Sync>;

/// Answers each `orders-pending` request from `plain` (by its index and its `after`) and every stop read
/// with no rows.
struct Venue {
    plain: Plain,
    sent: Sent,
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
        if path.ends_with("/orders-algo-pending") {
            return Ok(envelope(Vec::new()));
        }
        assert!(path.ends_with("/orders-pending"), "an order read only: {path}");
        let params: Vec<(String, Value)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let after = value(&params, "after").and_then(Value::as_str).map(String::from);
        let mut sent = self.sent.lock().unwrap();
        let n = sent.len();
        sent.push(params);
        drop(sent);
        Ok((self.plain)(n, after.as_deref()))
    }

    vike_bridge_core::no_public_call!("the recon client must use the SIGNED lane, not public");
}

fn client(
    plain: impl Fn(usize, Option<&str>) -> Value + Send + Sync + 'static,
) -> (ReconcileClientOkx<Venue>, Sent) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let venue = Venue { plain: Box::new(plain), sent: Arc::clone(&sent) };
    let client = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, || 0),
        venue,
        "https://unused.invalid",
        "BTC-USDT-SWAP",
        0.01,
    )
    .with_trailing(false);
    (client, sent)
}

fn envelope(rows: Vec<Value>) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

/// The id of the i-th order: descending, as a newest-first list hands them.
fn ord_id(i: usize) -> String {
    (700_000 - i).to_string()
}

fn page(rows: std::ops::Range<usize>) -> Value {
    envelope(
        rows.map(|i| {
            json!({
                "instId": "BTC-USDT-SWAP", "ordId": ord_id(i), "clOrdId": format!("c{i}"),
                "side": "buy", "ordType": "limit", "sz": "1", "accFillSz": "0", "avgPx": "",
                "state": "live", "px": "100", "uTime": "1700000000000"
            })
        })
        .collect(),
    )
}

fn value<'a>(params: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// A full page is followed by its last `ordId` and the pages are joined in order. Red before the fix: the
/// read asked one page with no `limit` and reported the first 100 orders as the whole list.
#[test]
fn a_full_page_of_plain_orders_is_followed_by_its_last_ord_id() {
    let (c, sent) = client(|n, after| match (n, after) {
        (0, None) => page(0..100),
        (1, Some(a)) if a == ord_id(99) => page(100..103),
        other => panic!("asked {other:?}"),
    });
    let reports = c.fetch_order_status_reports(0).expect("report");
    let order_client_ids: Vec<String> =
        reports.iter().map(|r| r.order_client_id.clone().expect("an order client id")).collect();
    assert_eq!(
        order_client_ids,
        (0..103).map(|i| format!("c{i}")).collect::<Vec<_>>(),
        "in page order"
    );
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2, "a short page ends the walk");
    assert_eq!(value(&sent[0], "instId"), Some(&json!("BTC-USDT-SWAP")));
    assert_eq!(
        value(&sent[0], "limit"),
        Some(&json!("100")),
        "the venue's maximum, asked outright"
    );
    assert_eq!(value(&sent[0], "after"), None, "the first page has no cursor");
}

/// A walk that cannot finish fails the report instead of returning what it has: an `ordId` that comes back
/// on the next page (a venue that ignored `after`) and a chain past the page cap.
#[test]
fn a_plain_walk_that_cannot_finish_fails_the_report() {
    let (c, _) = client(|_, _| page(0..100));
    let why = c.fetch_order_status_reports(0).expect_err("a repeated ordId");
    assert!(why.contains("ordId") && why.contains("second page"), "{why}");

    let (c, sent) = client(|n, _| page(n * 100..n * 100 + 100));
    let why = c.fetch_order_status_reports(0).expect_err("the cap");
    assert!(why.contains("still paging after 40 pages of 100 rows"), "{why}");
    assert_eq!(sent.lock().unwrap().len(), 40);
}
