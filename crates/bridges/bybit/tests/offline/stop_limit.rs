//! **The bybit STOP-LIMIT wire and the kinds the bridge refuses by name** (offline: no network).
//!
//! * a `stop_limit` on a derivative book is ONE conditional `Limit` on `/v5/order/create`: the limit's
//!   `timeInForce` and `price`, then the stop's `triggerPrice`, `triggerDirection` from the side (a BUY fires
//!   on a RISE, `1`; a SELL on a FALL, `2`) and `triggerBy` (`LastPrice` unless asked), every key in the order
//!   `crates/bridges/bybit/src/execution/rest/mod.rs`'s `build_order_params` writes it;
//! * a `market_to_limit` and a word outside the known kinds, which the builder would send as a plain `Market`,
//!   are refused by name before anything is signed, on the single and the batch door.
//!
//! The spot pair's stop-limit is `spot_conditional.rs`'s, the take-profits `take_profit.rs`'s. The venue's echo
//! of what this builds is `tests/bybit_smokes/bybit_stop_limit_smoke.rs`'s.

use std::cell::RefCell;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::{PATH_ORDER_CREATE, VenueRestBybit};
use vike_bybit::history::Category;
use vike_bybit::signer::BybitV5Signer;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties, TriggerBy};
use vike_secrets::credentials::Credentials;

/// One signed call: its path and its params.
type Call = (String, Vec<(String, Value)>);

/// Records every signed call and answers each as an accepted order.
#[derive(Default)]
pub(crate) struct Wire(pub(crate) RefCell<Vec<Call>>);

impl RestTransportJson for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.0.borrow_mut().push((path.to_string(), owned));
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"orderId": "oid-1"}}))
    }
}

pub(crate) fn client(symbol: &str, category: Category) -> VenueRestBybit<Wire> {
    VenueRestBybit {
        category,
        signer: BybitV5Signer::new(&Credentials::for_test("k", "s", None), || 0),
        transport: Wire::default(),
        base_url: "https://unused.invalid".to_string(),
        mainnet: false,
        symbol: symbol.to_string(),
        properties: SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() },
        leverage: 2.0,
    }
}

fn stop_limit(side: i32, trigger: f64, limit: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: "sl1".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 0.002,
        order_type: OrderType::StopLimit,
        price: Some(limit),
        trigger_price: Some(trigger),
        ..Default::default()
    }
}

pub(crate) fn owned(params: Vec<(&'static str, Value)>) -> Vec<(String, Value)> {
    params.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn expected(
    side: &str,
    limit: &str,
    trigger: &str,
    direction: i64,
    reduce_only: bool,
) -> Vec<(String, Value)> {
    owned(vec![
        ("category", json!("linear")),
        ("symbol", json!("BTCUSDT")),
        ("side", json!(side)),
        ("orderType", json!("Limit")),
        ("qty", json!("0.002")),
        ("orderLinkId", json!("sl1")),
        ("positionIdx", json!(0)),
        ("reduceOnly", json!(reduce_only)),
        ("timeInForce", json!("GTC")),
        ("price", json!(limit)),
        ("triggerPrice", json!(trigger)),
        ("triggerDirection", json!(direction)),
        ("triggerBy", json!("LastPrice")),
    ])
}

#[test]
fn a_stop_limit_is_one_conditional_limit_firing_the_way_its_side_does() {
    let c = client("BTCUSDT", Category::Linear);
    let sell = owned(c.build_order_params(&stop_limit(-1, 58000.3, 57990.1)));
    assert_eq!(sell, expected("Sell", "57990.1", "58000.3", 2, false), "a SELL fires on a fall");

    let mut buy = stop_limit(1, 62000.7, 62010.9);
    buy.reduce_only = true;
    assert_eq!(
        owned(c.build_order_params(&buy)),
        expected("Buy", "62010.9", "62000.7", 1, true),
        "a BUY fires on a rise"
    );

    // a requested trigger source is the venue's word for it, as for a stop
    buy.trigger_by = Some(TriggerBy::Mark);
    let params = owned(c.build_order_params(&buy));
    let by = params.iter().find(|(k, _)| k == "triggerBy").map(|(_, v)| v.clone());
    assert_eq!(by, Some(json!("MarkPrice")));
}

/// The real submit door sends exactly the builder's params to the create endpoint and reads the accept.
#[test]
fn the_submit_door_sends_the_stop_limit_and_reads_the_accept() {
    let c = client("BTCUSDT", Category::Linear);
    let events = c.submit_order(&stop_limit(-1, 58000.3, 57990.1));
    let Some(Event::OrderAccepted(accepted)) = events.last() else { panic!("{events:?}") };
    assert_eq!(accepted.venue_order_id.as_deref(), Some("oid-1"));
    let calls = c.transport.0.borrow();
    let [(path, params)] = calls.as_slice() else { panic!("one signed call: {calls:?}") };
    assert_eq!(path, PATH_ORDER_CREATE);
    assert_eq!(*params, expected("Sell", "57990.1", "58000.3", 2, false));
}

pub(crate) fn rejected(events: &[Event]) -> String {
    match events {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => r.reason.to_string(),
        other => panic!("submitted, then rejected: {other:?}"),
    }
}

/// A kind the builder does not build would leave as a `Market` (its `orderType` has two words) and fill at
/// once; it is refused by name instead, and nothing is signed.
#[test]
fn a_kind_the_builder_does_not_build_is_refused_by_name_never_sent_as_market() {
    let c = client("BTCUSDT", Category::Linear);
    for kind in [OrderType::MarketToLimit, OrderType::Unrecognized("trailing_stop".into())] {
        let mut request = stop_limit(-1, 58000.3, 57990.1);
        request.order_type = kind.clone();
        let reason = rejected(&c.submit_order(&request));
        assert_eq!(
            reason,
            format!("bybit: a {kind} order is not built by this bridge; nothing was sent")
        );
        let events = c.submit_batch(&[request]);
        assert!(events.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{kind}: {events:?}");
    }
    assert!(c.transport.0.borrow().is_empty(), "nothing is signed: {:?}", c.transport.0.borrow());
}
