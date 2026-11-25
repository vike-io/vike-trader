//! **Every bybit REST read and write says what failed when the venue's message is EMPTY** (offline: a scripted
//! transport, no network, nothing `#[ignore]`d). The answer replayed is the demo host's recorded one, `retCode 10001`
//! with an empty `retMsg` (measured on the fee-rate read 2026-11-08,
//! `crates/bridges/bybit/tests/bybit_smokes/bybit_inverse_smoke.rs`'s `probe_fee_rate`, and again for both demo
//! accounts at the CI box's v0.1.102 start, 2026-11-14), handed back as the HTTP 200 envelope it comes in, so the client's
//! own `unwrap_envelope` raises it. Every error names the request, the code and `(empty message)`:
//! `crates/bridges/bybit/src/transport/client.rs`'s `FailedRequest`, the shape the fee-rate read already had.
//!
//! Base: the reconcile reads (orders, fills, positions, balance), the operator's reads, the funding poll, an order's
//! reject, a cancel's and an amend's refusal and the leverage write said the venue's message alone, which is EMPTY
//! here; the leverage and mode reads said the code with nothing after it.

use serde_json::{Value, json};
use vike_bridge_core::leverage_read::LeverageReader;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::ReconcileClientBybit;
use vike_bybit::account::leverage::LeverageReaderBybit;
use vike_bybit::execution::funding::BybitFundingPoller;
use vike_bybit::reconcile::venue_read::{read_balance, read_open_orders};
use vike_bybit::signer::BybitV5Signer;
use vike_execution::recon::ReconcileClient;
use vike_model::OrderRequest;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// What every error below ends with.
const EMPTY: &str = "venue error 10001 (empty message)";

/// Answers EVERY request as the demo host answers a refused one: HTTP 200, `retCode 10001`, an EMPTY `retMsg`.
struct EmptyRefusal;

impl RestTransportJson for EmptyRefusal {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        Ok(json!({"retCode": 10001, "retMsg": "", "result": {}, "retExtInfo": {}, "time": 1}))
    }
}

/// The signer every client below signs with; the symbols they are built on are USER names (decision 0290), read off
/// the held kept list (`BTCUSDT.P` is the linear `BTCUSDT`, `BTCUSD.P` the inverse `BTCUSD`).
fn signer() -> BybitV5Signer {
    vike_bybit::history::kept_list_for_tests();
    BybitV5Signer::new(&Credentials::for_test("k", "s", None), || 0)
}

/// `request`'s failure, as every bybit error says it.
fn said(request: &str) -> String {
    format!("{request}: {EMPTY}")
}

fn limit(order_client_id: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

fn rejected(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_reconcile_reads_name_the_request_and_the_code() {
    let c = ReconcileClientBybit::new(signer(), EmptyRefusal, "https://x", "BTCUSDT.P");
    assert_eq!(
        c.fetch_order_status_reports(0).unwrap_err(),
        said("bybit GET /v5/order/realtime (category linear, symbol BTCUSDT, limit 50)")
    );
    assert_eq!(
        c.fetch_fill_reports(0).unwrap_err(),
        said("bybit GET /v5/execution/list (category linear, symbol BTCUSDT, limit 100)")
    );
    assert_eq!(
        c.fetch_position_status_reports().unwrap_err(),
        said("bybit GET /v5/position/list (category linear, symbol BTCUSDT)")
    );
    assert_eq!(
        c.fetch_balance().unwrap_err().text,
        said("bybit GET /v5/account/wallet-balance (accountType UNIFIED)")
    );
    // A coin-margined mount asks the account's version first.
    let inverse = ReconcileClientBybit::new(signer(), EmptyRefusal, "https://x", "BTCUSD.P");
    assert_eq!(
        inverse.fetch_balance().unwrap_err().text,
        format!(
            "{}: the wallet of the inverse book is not guessed, so its balance is not read",
            said("bybit GET /v5/account/info")
        )
    );
}

#[test]
fn the_operators_reads_name_the_request_and_the_code() {
    assert_eq!(
        read_balance(&EmptyRefusal, &signer(), "https://x").unwrap_err().text(),
        said("bybit GET /v5/account/info")
    );
    let c = ReconcileClientBybit::new(signer(), EmptyRefusal, "https://x", "BTCUSDT.P");
    assert_eq!(
        read_open_orders(&c).unwrap_err(),
        said("bybit GET /v5/order/realtime (category linear, settleCoin USDT, limit 50)")
    );
}

#[test]
fn the_leverage_reads_name_the_request_and_the_code() {
    let reader = LeverageReaderBybit::new(signer(), EmptyRefusal, "https://x", "BTCUSDT.P");
    assert_eq!(reader.check_account().unwrap_err(), said("bybit GET /v5/account/info"));
    assert_eq!(
        reader.held().unwrap_err(),
        said("bybit GET /v5/position/list (category linear, symbol BTCUSDT)")
    );
}

#[test]
fn the_order_writes_name_the_request_and_the_code() {
    let c = bybit_perp(&fixture(), EmptyRefusal);
    // A create: the terminal reject's reason (10001 is a definite refusal, nothing is re-queried).
    assert_eq!(
        rejected(&c.submit_order(&limit("c1"))),
        [said("bybit POST /v5/order/create (category linear, symbol BTCUSDT, orderLinkId c1)")]
    );
    // A batch: each order's.
    assert_eq!(
        rejected(&c.submit_batch(&[limit("a"), limit("b")])),
        [
            said(
                "bybit POST /v5/order/create-batch (category linear, symbol BTCUSDT, orderLinkId a)"
            ),
            said(
                "bybit POST /v5/order/create-batch (category linear, symbol BTCUSDT, orderLinkId b)"
            ),
        ]
    );
    // A cancel: the shared loop's `OrderCancelRejected` says this message; the code is kept for every judgement.
    let refused = c.cancel_order("c1").unwrap_err();
    assert_eq!(refused.code, 10001);
    assert_eq!(
        refused.msg,
        said("bybit POST /v5/order/cancel (category linear, symbol BTCUSDT, orderLinkId c1)")
    );
    // An amend: the non-terminal advisory's reason.
    match &c.modify_order("c1", Some(0.02), None)[..] {
        [Event::OrderModifyRejected(m)] => assert!(
            m.reason.starts_with(
                "bybit POST /v5/order/amend (category linear, symbol BTCUSDT, orderLinkId c1, qty 0.02"
            ) && m.reason.ends_with(&format!("): {EMPTY}")),
            "{}",
            m.reason
        ),
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
    // The leverage write, as the start's WARN and the per-symbol write's refusal say it.
    assert_eq!(
        c.set_leverage().unwrap_err().to_string(),
        said(
            "bybit POST /v5/position/set-leverage (category linear, symbol BTCUSDT, buyLeverage 2.0, \
             sellLeverage 2.0)"
        )
    );
}

#[test]
fn the_funding_poll_names_the_request_and_the_code() {
    let rest = bybit_perp(&fixture(), EmptyRefusal);
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT.P", 0);
    assert_eq!(
        poller.poll().unwrap_err(),
        said("bybit GET /v5/account/transaction-log (category linear, type SETTLEMENT)")
    );
}
