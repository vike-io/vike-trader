//! **The delivery guard at send** (B3.02: `vike_venues::venue_delivery_guard::delivery_verdict`, okx's rule for every
//! venue). On a dated contract (the delivery day its USER name states, 08:00 UTC) an OPENING order inside its last day
//! and every order after the delivery are a terminal `OrderRejected` with nothing on the wire, on the single submit and
//! per order inside the native batch; a reduce-only order is still sent until the delivery, and a far contract trades.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::execution_drive;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::VenueRestBybit;
use vike_bybit::signer::BybitV5Signer;
use vike_model::events::Event;
use vike_model::{MS_PER_DAY, MS_PER_HOUR, OrderRequest, SymbolProperties};
use vike_secrets::credentials::Credentials;

/// Counts the wire calls and acks every order (one id per batch member).
#[derive(Default)]
struct Acks {
    calls: Mutex<usize>,
}

impl RestTransportJson for Acks {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        *self.calls.lock().unwrap() += 1;
        let n = params
            .iter()
            .find(|(k, _)| *k == "request")
            .and_then(|(_, v)| v.as_array().map(Vec::len))
            .unwrap_or(1);
        let acks: Vec<Value> = (0..n).map(|i| json!({ "orderId": format!("oid-{i}") })).collect();
        let exts: Vec<Value> = (0..n).map(|_| json!({ "code": 0, "msg": "OK" })).collect();
        Ok(json!({
            "retCode": 0, "retMsg": "OK",
            "result": { "orderId": "oid-0", "list": acks },
            "retExtInfo": { "list": exts },
        }))
    }
}

fn client() -> VenueRestBybit<Acks> {
    VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        transport: Acks::default(),
        base_url: "https://unused.invalid".to_string(),
        mainnet: false,
        symbol: "ETHUSDT-01NOV26".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    }
}

/// The USER name of the dated `ETHUSDT` contract delivered on the UTC day `at_ms` falls on (`ETHUSDT_YYMMDD`).
fn dated(at_ms: i64) -> String {
    let (y, m, d) = vike_utility::calendar::civil_from_days(at_ms.div_euclid(MS_PER_DAY));
    format!("ETHUSDT_{:02}{m:02}{d:02}", y - 2000)
}

/// The contract delivered at the next 08:00 UTC: between zero and 24 h away, inside its last day.
fn inside_the_window() -> String {
    dated(vike_model::now_ms() - 8 * MS_PER_HOUR + MS_PER_DAY)
}

fn order(order_client_id: &str, symbol: &str, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        reduce_only,
        ..execution_drive::limit("bybit", order_client_id, symbol, 0.01, 3000.0)
    }
}

fn rejected(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.order_client_id.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_opening_order_inside_the_last_day_is_refused_and_nothing_is_sent() {
    let bybit = client();
    let events = VenueRest::submit_order(&bybit, &order("c-open", &inside_the_window(), false));
    execution_drive::assert_denied_pair(
        &events,
        "c-open",
        "takes no OPENING order inside its last day",
    );
    let Event::OrderRejected(refused) = &events[1] else { unreachable!("assert_denied_pair") };
    assert_eq!(refused.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
    assert_eq!(*bybit.transport.calls.lock().unwrap(), 0, "nothing reached the wire");
}

#[test]
fn a_reduce_only_order_inside_the_last_day_is_sent() {
    let bybit = client();
    let events = VenueRest::submit_order(&bybit, &order("c-close", &inside_the_window(), true));
    assert!(rejected(&events).is_empty(), "{events:?}");
    assert_eq!(*bybit.transport.calls.lock().unwrap(), 1);
}

#[test]
fn every_order_on_a_delivered_contract_is_refused() {
    let bybit = client();
    let delivered = dated(vike_model::now_ms() - 2 * MS_PER_DAY);
    for (order_client_id, reduce_only) in [("c-open", false), ("c-close", true)] {
        let events =
            VenueRest::submit_order(&bybit, &order(order_client_id, &delivered, reduce_only));
        execution_drive::assert_denied_pair(&events, order_client_id, "was delivered");
    }
    assert_eq!(*bybit.transport.calls.lock().unwrap(), 0);
}

/// Per order inside the batch: the opening order inside the window is refused, its siblings batch.
#[test]
fn a_batch_member_inside_the_last_day_is_refused_and_its_siblings_are_sent() {
    let bybit = client();
    let far = dated(vike_model::now_ms() + 30 * MS_PER_DAY);
    let requests = vec![
        order("c-far", &far, false),
        order("c-open", &inside_the_window(), false),
        order("c-close", &inside_the_window(), true),
    ];
    let events = bybit.submit_batch(&requests);
    assert_eq!(rejected(&events), ["c-open"], "{events:?}");
    assert_eq!(*bybit.transport.calls.lock().unwrap(), 1, "one wire chunk for the two others");
}
