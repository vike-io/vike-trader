//! **The family's reconcile fill read walks every page of the window** (owner ruling F-2; offline: a
//! scripted `RestTransport`). `myTrades` / `userTrades` answer at most 1000 rows a request; the read used to
//! be ONE page with no `limit` (the venue's 500), reported as the whole window, so a busy window's 501st
//! fill was never reported. Now the first page is `startTime` + `limit=1000`, a FULL page is followed by
//! `fromId` one past its greatest trade id (alone: the venues take `fromId` with no `startTime`), and a
//! chain that cannot finish fails the read. Driven on the spot lane, which reads its fills and nothing else.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_binance::family::recon::ReconcileClientBinanceFamily;
use vike_binance::reconcile::BINANCE_RECONCILE;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_execution::recon::ReconcileClient;

use crate::time_in_force_gate::NullSigner;

type Sent = Arc<Mutex<Vec<Vec<(String, String)>>>>;

/// One page's answer by the `fromId` it was asked from.
type Pages = Box<dyn Fn(Option<i64>) -> Value + Send + Sync>;

/// Answers each `myTrades` request from `pages` (by its `fromId`, `None` for the first page).
struct Venue {
    pages: Pages,
    sent: Sent,
}

impl RestTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        assert_eq!(path, BINANCE_RECONCILE.paths.spot_my_trades, "a fill read only");
        let params: Vec<(String, String)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let from = params.iter().find(|(k, _)| k == "fromId").map(|(_, v)| v.parse().unwrap());
        self.sent.lock().unwrap().push(params);
        Ok((self.pages)(from))
    }

    fn public(&self, _: &str, path: &str, _: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("a reconcile read is signed, never public: {path}")
    }
}

fn client(
    pages: impl Fn(Option<i64>) -> Value + Send + Sync + 'static,
) -> (ReconcileClientBinanceFamily<NullSigner, Venue>, Sent) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let venue = Venue { pages: Box::new(pages), sent: Arc::clone(&sent) };
    (
        ReconcileClientBinanceFamily::spot(
            BINANCE_RECONCILE,
            NullSigner,
            venue,
            "https://x",
            "BTCUSDT",
        ),
        sent,
    )
}

/// Trades with ids `ids` (spot `myTrades` shape).
fn page(ids: std::ops::Range<i64>) -> Value {
    Value::Array(
        ids.map(|id| {
            json!({
                "symbol": "BTCUSDT", "id": id, "orderId": 9, "price": "100", "qty": "1",
                "commission": "0", "commissionAsset": "USDT", "time": 5, "isBuyer": false,
                "isMaker": false
            })
        })
        .collect(),
    )
}

/// A full page is followed by `fromId` one past its greatest id, with no `startTime`, and every page's
/// fills are reported. Red before the fix: one request with no `limit`, the first page as the window.
#[test]
fn a_full_page_of_trades_is_followed_from_one_past_its_greatest_id() {
    let (c, sent) = client(|from| match from {
        None => page(1..1001),
        Some(1001) => page(1001..1004),
        other => panic!("asked from {other:?}"),
    });
    let fills = c.fetch_fill_reports(5).expect("the window");
    assert_eq!(fills.len(), 1003, "every page's fills");
    let pairs = |p: &[(&str, &str)]| -> Vec<(String, String)> {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert_eq!(
        *sent.lock().unwrap(),
        [
            pairs(&[("symbol", "BTCUSDT"), ("startTime", "5"), ("limit", "1000")]),
            pairs(&[("symbol", "BTCUSDT"), ("fromId", "1001"), ("limit", "1000")]),
        ]
    );
}

/// A walk that cannot finish fails the read: a venue that ignored `fromId` (the same page again) and the
/// page cap.
#[test]
fn a_trade_walk_that_cannot_finish_fails_the_read() {
    let (c, _) = client(|_| page(1..1001));
    let why = c.fetch_fill_reports(0).expect_err("the same page again");
    assert!(why.contains("ended no further on"), "{why}");

    let (c, sent) = client(|from| {
        let start = from.unwrap_or(1);
        page(start..start + 1000)
    });
    let why = c.fetch_fill_reports(0).expect_err("the cap");
    assert!(why.contains("still paging after 10 pages of 1000 rows"), "{why}");
    assert_eq!(sent.lock().unwrap().len(), 10);
}
