//! A cancel of an order the venue no longer holds reads `OrderCancelAck::AlreadyGone`, from the reply the DEMO
//! host really sends. Each envelope below is the venue's answer, byte for byte (minus the `time` stamp), to a
//! second `POST /v5/order/cancel` by `orderLinkId`, account `bybit/DEMO`, 2026-11-08: the order was placed far
//! from the book and cancelled through the node first (`crates/bridges/bybit/src/transport/error_codes.rs`'s
//! `cancel_says_gone` holds the measured set). The stub hands the envelope back as `Ok`, as
//! the shared reply engine (`vike_bridge_core::transport::reply`'s `classify_rest_reply`) does for an HTTP 200, so the client's `unwrap_envelope` raises the code.

use serde_json::{Value, json};
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::VenueRestBybit;
use vike_bybit::history::Category;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// Answers the cancel POST with `reply`, after checking it names the book and the order client id.
struct Stub {
    category: &'static str,
    reply: Value,
}

impl RestTransportJson for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        assert_eq!((method, path), ("POST", "/v5/order/cancel"));
        let p: Vec<(&str, &Value)> = params.iter().map(|(k, v)| (*k, v)).collect();
        assert_eq!(
            p,
            [
                ("category", &json!(self.category)),
                ("symbol", &json!("BTCUSDT")),
                ("orderLinkId", &json!("bcc1108L1"))
            ]
        );
        Ok(self.reply.clone())
    }
}

fn cancel(category: Category, wire: &'static str, reply: Value) -> OrderCancelAck {
    let rest =
        VenueRestBybit { category, ..bybit_perp(&fixture(), Stub { category: wire, reply }) };
    rest.cancel_order("bcc1108L1").expect("a gone order is an Ok")
}

/// `linear`: a cancelled limit, a `Deactivated` (cancelled) stop and an IOC the venue killed all answered this.
#[test]
fn linear_110001_is_already_gone() {
    let reply = json!({"retCode": 110001, "retMsg": "order not exists or too late to cancel",
        "result": {}, "retExtInfo": {}});
    assert_eq!(cancel(Category::Linear, "linear", reply), OrderCancelAck::AlreadyGone);
}

/// `spot`: a cancelled limit and an IOC the venue killed both answered this.
#[test]
fn spot_170213_is_already_gone() {
    let reply = json!({"retCode": 170213, "retMsg": "Order does not exist.", "result": {}, "retExtInfo": {}});
    assert_eq!(cancel(Category::Spot, "spot", reply), OrderCancelAck::AlreadyGone);
}
