//! `-2011 CANCEL_REJECTED` is an UMBRELLA code (`vike_binance::transport::error_codes`' module doc): "Unknown order
//! sent." means the order is gone, "This account may not place or cancel orders." is a refusal over a
//! LIVE order. Swallowing every `-2011` reported that live order CANCELED. Only the words that say the
//! order is not there are "gone"; every other `-2011` is an error, and on a status read it says nothing.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_binance::execution::rest::perp::VenueRestBinancePerp;
use vike_binance::execution::rest::spot::VenueRestBinanceSpot;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};

use crate::time_in_force_gate::NullSigner;

const GONE: &str = "Unknown order sent.";
const REFUSED: &str = "This account may not place or cancel orders.";

type Answer = Box<dyn Fn(&str, &str) -> Result<Value, VenueApiError> + Send + Sync>;

/// Answers by `(method, path)` and records the wire.
struct Script {
    answer: Answer,
    wire: Mutex<Vec<String>>,
}

impl Script {
    fn new(
        answer: impl Fn(&str, &str) -> Result<Value, VenueApiError> + Send + Sync + 'static,
    ) -> Self {
        Self { answer: Box::new(answer), wire: Mutex::new(Vec::new()) }
    }
}

impl RestTransport for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        self.wire.lock().unwrap().push(format!("{method} {path}"));
        (self.answer)(method, path)
    }
    vike_bridge_core::no_public_call!();
}

fn err(msg: &str) -> VenueApiError {
    VenueApiError { code: -2011, msg: msg.to_string() }
}

fn perp(
    answer: impl Fn(&str, &str) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> VenueRestBinancePerp<NullSigner, Script> {
    VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: Script::new(answer),
        base_url: "https://stub".into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(
    answer: impl Fn(&str, &str) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> VenueRestBinanceSpot<NullSigner, Script> {
    VenueRestBinanceSpot {
        link_id: None,
        signer: NullSigner,
        transport: Script::new(answer),
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() },
        base_asset: "BTC".into(),
    }
}

#[test]
fn a_spot_cancel_swallows_only_the_words_that_say_the_order_is_gone() {
    assert!(spot(|_, _| Err(err(GONE))).cancel_order("c1").is_ok());
    assert!(spot(|_, _| Err(err("Order does not exist."))).cancel_order("c1").is_ok());
    for words in [REFUSED, "Order was not canceled due to cancel restrictions.", ""] {
        let e = spot(move |_, _| Err(err(words))).cancel_order("c1").expect_err("may be live");
        assert_eq!(e.code, -2011, "{words}");
    }
}

/// The regular endpoint's "unknown" goes on to the algo endpoint; its refusal does not: the order is
/// real, and the algo endpoint's "unknown" would have read as gone.
#[test]
fn a_perp_cancel_retries_on_the_algo_endpoint_only_after_the_words_that_say_unknown() {
    let c = perp(|_, _| Err(err(GONE)));
    assert!(c.cancel_order("c1").is_ok());
    assert_eq!(
        *c.transport.wire.lock().unwrap(),
        ["DELETE /fapi/v1/order", "DELETE /fapi/v1/algoOrder"]
    );

    let c = perp(|_, path| match path {
        "/fapi/v1/order" => Err(err(REFUSED)),
        other => panic!("a refusal must not be retried on {other}"),
    });
    assert_eq!(c.cancel_order("c1").expect_err("live").msg, REFUSED);
    assert_eq!(*c.transport.wire.lock().unwrap(), ["DELETE /fapi/v1/order"]);
}

/// The stop's own endpoint refuses (the account, not the id): no `OrderCanceled` over a resting stop, and
/// the order client id stays remembered as a stop.
#[test]
fn an_algo_cancel_refused_in_other_words_is_an_error_and_keeps_the_stop() {
    let c = perp(|_, path| match path {
        "/fapi/v1/order" => Err(err(GONE)),
        _ => Err(err(REFUSED)),
    });
    assert_eq!(c.cancel_order("sl1").expect_err("the stop rests").msg, REFUSED);

    let c = perp(|_, _| Err(err(REFUSED)));
    c.algo_order_client_ids.insert("sl1");
    assert_eq!(c.cancel_order("sl1").expect_err("the stop rests").msg, REFUSED);
    assert!(c.algo_order_client_ids.contains("sl1"), "a refused cancel leaves it a stop");
    assert_eq!(*c.transport.wire.lock().unwrap(), ["DELETE /fapi/v1/algoOrder"]);
}

#[test]
fn a_batch_cancel_element_refused_in_other_words_is_the_batchs_error() {
    let c = perp(|_, _| Ok(json!([{"code": -2011, "msg": REFUSED}])));
    assert_eq!(c.cancel_batch(&["c1".to_string()]).expect_err("live").msg, REFUSED);
    assert_eq!(*c.transport.wire.lock().unwrap(), ["DELETE /fapi/v1/batchOrders"]);
}

/// A status read: `-2011` in the words that say unknown is ABSENT (the terminal reject of a confirm); in
/// any other words it says nothing about the order, so a live order is not rejected.
#[test]
fn a_status_read_answering_minus_2011_in_other_words_says_nothing() {
    assert!(matches!(
        spot(|_, _| Err(err(GONE))).confirm_order("c1", 5)[..],
        [Event::OrderRejected(_)]
    ));
    assert!(spot(|_, _| Err(err(REFUSED))).confirm_order("c1", 5).is_empty());
    assert!(matches!(
        perp(|_, _| Err(err(GONE))).confirm_order("c1", 5)[..],
        [Event::OrderRejected(_)]
    ));
    assert!(perp(|_, _| Err(err(REFUSED))).confirm_order("c1", 5).is_empty());
}

/// A spot cancel-replace that failed, then a status read refused in other words: the order's state is
/// unknown. It must not be booked gone.
#[test]
fn a_spot_modify_settled_by_a_refused_read_does_not_book_the_order_gone() {
    let order = OrderRequest {
        order_client_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.005,
        order_type: vike_model::OrderType::Limit,
        price: Some(60_000.0),
        ..Default::default()
    };
    let c = spot(|method, _| match method {
        "POST" => Err(VenueApiError { code: -2022, msg: "the cancel failed".to_string() }),
        _ => Err(err(REFUSED)),
    });
    let events = c.modify_order(&order, None, Some(61_000.0));
    assert!(matches!(events[..], [Event::OrderModifyRejected(_)]), "{events:?}");
}
