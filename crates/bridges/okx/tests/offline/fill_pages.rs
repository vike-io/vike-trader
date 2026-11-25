//! **The reconcile fill read walks every page of the window** (owner ruling F-2; offline: a scripted
//! `RestTransportJson`). `trade/fills` lists at most 100 rows a request, newest first, and `after` (a `billId`)
//! asks for the records earlier than it. The read used to be ONE page, reported as the whole window, so a
//! busy window's 101st fill was never reported (no `MissingFill`, no adoption). A page is full by its ROW
//! count (a liquidation row the parser drops still fills it), a short page ends the walk, and a chain that
//! cannot finish fails the read.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::ReconcileClient;
use vike_okx::ReconcileClientOkx;
use vike_okx::signer::OkxV5Signer;
use vike_secrets::credentials::Credentials;

/// The params of every `trade/fills` request, in order.
type Sent = Arc<Mutex<Vec<Vec<(String, Value)>>>>;

/// One page's answer by the `after` it was asked with.
type Fills = Box<dyn Fn(Option<&str>) -> Value + Send + Sync>;

/// Answers each `trade/fills` request from `fills` (by its `after`); no stop ever fired, so the stop
/// history the pass reads to name fills answers no rows.
struct Venue {
    fills: Fills,
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
        if path.ends_with("/orders-algo-history") {
            return Ok(envelope(Vec::new()));
        }
        assert!(path.ends_with("/trade/fills"), "a fill read only: {path}");
        let params: Vec<(String, Value)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let after = params
            .iter()
            .find(|(k, _)| k == "after")
            .and_then(|(_, v)| v.as_str().map(String::from));
        self.sent.lock().unwrap().push(params);
        Ok((self.fills)(after.as_deref()))
    }

    vike_bridge_core::no_public_call!("the recon client must use the SIGNED lane, not public");
}

fn client(
    fills: impl Fn(Option<&str>) -> Value + Send + Sync + 'static,
) -> (ReconcileClientOkx<Venue>, Sent) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let venue = Venue { fills: Box::new(fills), sent: Arc::clone(&sent) };
    let client = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, || 0),
        venue,
        "https://unused.invalid",
        "BTC-USDT-SWAP",
        0.01,
    );
    (client, sent)
}

fn envelope(rows: Vec<Value>) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

/// The bill id of the i-th row: descending, as a newest-first list hands them.
fn bill_id(i: usize) -> String {
    (500_000 - i).to_string()
}

/// Rows `rows`; the row at `liquidation` (if inside) is a liquidation execution the parser drops.
fn page(rows: std::ops::Range<usize>, liquidation: usize) -> Value {
    envelope(
        rows.map(|i| {
            let sub_type = if i == liquidation { "100" } else { "1" };
            json!({
                "instId": "BTC-USDT-SWAP", "tradeId": format!("t{i}"), "billId": bill_id(i),
                "ordId": format!("o{i}"), "clOrdId": format!("c{i}"), "side": "buy", "fillPx": "50000",
                "fillSz": "1", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "subType": sub_type,
                "ts": "1700000000000"
            })
        })
        .collect(),
    )
}

/// A full page (one row of it a liquidation the parser drops) is followed by its last `billId`, and every
/// page's fills are reported. Red before the fix: one request with no `limit`, 99 fills reported.
#[test]
fn a_full_page_of_fills_is_followed_by_its_last_bill_id() {
    let (c, sent) = client(|after| match after {
        None => page(0..100, 0),
        Some(a) if a == bill_id(99) => page(100..102, usize::MAX),
        other => panic!("asked after {other:?}"),
    });
    let fills = c.fetch_fill_reports(1_000).expect("the window");
    assert_eq!(fills.len(), 99 + 2, "every page's trades, the liquidation row dropped");
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2, "a short page ends the walk");
    let first: Value = sent[0].iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    assert_eq!(first, json!({"instId": "BTC-USDT-SWAP", "begin": "1000", "limit": "100"}));
}

/// A walk that cannot finish fails the read: a venue that ignored `after` (the same `billId` again) and a
/// full page with no `billId` to ask the next one from.
#[test]
fn a_fill_walk_that_cannot_finish_fails_the_read() {
    let (c, _) = client(|_| page(0..100, usize::MAX));
    let why = c.fetch_fill_reports(0).expect_err("the same page again");
    assert!(why.contains("billId") && why.contains("second page"), "{why}");

    let (c, _) = client(|_| {
        let mut rows = page(0..100, usize::MAX);
        rows["data"][99].as_object_mut().unwrap().remove("billId");
        rows
    });
    let why = c.fetch_fill_reports(0).expect_err("no cursor");
    assert!(why.contains("no billId"), "{why}");
}
