//! `venue_read` — an OPERATOR's read of an OANDA account, asked of the venue now: its pending orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue open-orders oanda` prints it). Nothing is folded and nothing changes at the venue.
//!
//! `GET /v3/accounts/{id}/pendingOrders` is ACCOUNT-WIDE (every instrument's pending orders, unpaged), read
//! through the reconcile client's own 401-tolerant read on the crate's Bearer transport and parsed by its own
//! parser (`crate::reconcile`'s `read_past_401` and `parse_order_reports`, once per instrument the body
//! names), so the two reads cannot disagree about a row: a LIMIT's `price` is its price, a STOP's or a
//! MARKET_IF_TOUCHED's its TRIGGER, and our order client id is the order's `clientExtensions.id`.
//!
//! ⚠ **A trade's own exit is not a row.** A `STOP_LOSS`, `TAKE_PROFIT` or trailing stop attached to an open trade
//! (`tradeID`) names no instrument, no size and no side, and the parser drops it (reconcile tracks entry orders;
//! this bridge places none of them). Such a row marks the list `truncated` ("the venue holds more than these"),
//! never a silent cut. The balance and the positions are not offered.

use std::time::Duration;

use serde_json::Value;
use vike_model::{VenueRead, VenueReadOpenOrder};
use vike_utility::errors::CarriedRefusal;

use crate::config::OandaConfig;
use crate::reconcile::{parse_order_reports, read_past_401, require_array};
use crate::refusals::OandaVenueReadRefusal;
use crate::transport::OandaTransport;

/// The account's pending orders, on a fresh Bearer transport of `config`'s; each 401 asked again after `pauses`.
pub(crate) fn open_orders(
    config: &OandaConfig,
    pauses: &[Duration],
) -> Result<VenueRead, CarriedRefusal> {
    let rest = OandaTransport::new(config.api_token.expose().clone());
    let path = format!("/v3/accounts/{}/pendingOrders", config.account_id);
    let body = read_past_401(&rest, &config.rest_base, &path, "", "open orders", pauses)
        .map_err(|e| OandaVenueReadRefusal::PendingUnread { why: e.to_string() }.carried())?;
    let (orders, truncated) = open_orders_from(&body)?;
    Ok(VenueRead::OpenOrders { orders, truncated })
}

/// A `pendingOrders` body → its rows, each under the user name of the instrument the venue names (`EUR_USD` is
/// `EURUSD`, `crate::names::user_name_or_wire`), cut at [`vike_model::MAX_VENUE_OPEN_ORDERS`]; `truncated` also when
/// the body holds a trade's exit (no instrument: module doc).
fn open_orders_from(body: &Value) -> Result<(Vec<VenueReadOpenOrder>, bool), String> {
    require_array(body, "orders", "open orders")?;
    let rows = body["orders"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut instruments: Vec<&str> = Vec::new();
    let mut trade_exits = false;
    for row in rows {
        match row.get("instrument").and_then(Value::as_str).filter(|i| !i.is_empty()) {
            Some(instrument) if !instruments.contains(&instrument) => instruments.push(instrument),
            Some(_) => {}
            None => trade_exits = true,
        }
    }
    let reports: Vec<_> = instruments
        .iter()
        .flat_map(|i| parse_order_reports(body, i, &crate::names::user_name_or_wire(i)))
        .collect();
    let (orders, cut) = vike_model::open_orders_read(reports);
    // A trade's exit names no instrument and is not among the rows: said as a cut (module doc).
    Ok((orders, trade_exits || cut))
}

#[cfg(test)]
mod tests {
    use vike_bridge_core::venue_adapter::VenueAdapter;
    use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
    use vike_model::{AccountTier, VenueReadKind, not_offered_words};

    use super::*;
    use crate::adapter::VenueAdapterOanda;

    /// Two instruments' pending orders in the v20 `pendingOrders` shape under `Accept-Datetime-Format: UNIX`
    /// (hand-written from the reference, not a capture): our BUY LIMIT on EUR_USD (`clientExtensions.id` is our
    /// order client id) and a SELL STOP on GBP_USD placed in the platform.
    const BODY: &str = r#"{"orders":[
        {"id":"101","createTime":"1700000000.000000000","type":"LIMIT","instrument":"EUR_USD",
         "units":"1500","price":"1.05000","timeInForce":"GTC","state":"PENDING",
         "clientExtensions":{"id":"S00A307k3x9q2m7d1p4ENLM"}},
        {"id":"102","createTime":"1700000005.000000000","type":"STOP","instrument":"GBP_USD",
         "units":"-40","price":"1.24120","timeInForce":"GTC","state":"PENDING"}
    ],"lastTransactionID":"102"}"#;

    /// A trade's stop-loss: no instrument, no units (the reference's `StopLossOrder`).
    const TRADE_EXIT: &str = r#"{"orders":[
        {"id":"103","createTime":"1700000009.000000000","type":"STOP_LOSS","tradeID":"99",
         "price":"1.04000","timeInForce":"GTC","state":"PENDING"}
    ]}"#;

    #[test]
    fn every_instruments_pending_order_is_listed_with_its_kind_trigger_and_our_id() {
        crate::test_support::kept_list_for_tests();
        let (orders, truncated) = open_orders_from(&serde_json::from_str(BODY).unwrap()).unwrap();
        assert!(!truncated);
        let (limit, stop) = (&orders[0], &orders[1]);
        assert_eq!((limit.symbol.as_str(), limit.venue_order_id.as_str()), ("EURUSD", "101"));
        assert_eq!((limit.side, limit.qty, limit.filled_qty), (1, 1500.0, 0.0));
        assert_eq!(
            (limit.price, limit.trigger_price, limit.order_type.as_str()),
            (Some(1.05), None, "limit")
        );
        assert_eq!(limit.order_client_id.as_deref(), Some("S00A307k3x9q2m7d1p4ENLM"));
        assert_eq!((limit.status.as_str(), limit.timestamp_ms), ("ACCEPTED", 1_700_000_000_000));
        assert_eq!((stop.symbol.as_str(), stop.side, stop.qty), ("GBPUSD", -1, 40.0));
        assert_eq!(
            (stop.price, stop.trigger_price, stop.order_type.as_str()),
            (None, Some(1.2412), "stop"),
            "a STOP's price is its TRIGGER"
        );
        assert_eq!(stop.order_client_id, None, "a platform order carries no id of ours");
    }

    #[test]
    fn a_trades_exit_marks_the_list_cut_and_a_body_without_orders_is_an_error() {
        let (orders, truncated) =
            open_orders_from(&serde_json::from_str(TRADE_EXIT).unwrap()).unwrap();
        assert!(
            orders.is_empty() && truncated,
            "a row this read cannot place is said, never dropped"
        );
        let empty = open_orders_from(&serde_json::json!({"orders": []})).unwrap();
        assert_eq!(empty, (Vec::new(), false), "nothing pending is an answer");
        assert!(open_orders_from(&serde_json::json!({})).is_err());
    }

    /// The read is ONE `GET …/pendingOrders` of the account, unfiltered.
    #[test]
    fn the_read_is_one_account_wide_pending_orders_get() {
        let (base, seen) = vike_log::loopback::serve_replies("", vec![(200, BODY.to_string())]);
        let config = OandaConfig {
            api_token: vike_model::secret::Secret::new("token".into()),
            account_id: "101-004-1-001".into(),
            rest_base: base,
            stream_base: String::new(),
        };
        let Ok(VenueRead::OpenOrders { orders, truncated: false }) = open_orders(&config, &[])
        else {
            panic!("the pending orders are read");
        };
        assert_eq!(orders.len(), 2);
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert!(
            requests[0].starts_with("GET /v3/accounts/101-004-1-001/pendingOrders "),
            "{requests:?}"
        );
    }

    /// Refused in words before any request: a kind this bridge does not read, an account with no pair, and a
    /// `live` account holding one (this build reaches no fxTrade host).
    #[test]
    fn a_read_that_cannot_be_made_is_refused_before_any_request() {
        let read = |fx: &AdapterFixture, tier, kind| {
            VenueAdapterOanda.venue_read(&fx.inputs(tier), kind).unwrap_err()
        };
        let keyless = AdapterFixture::new(&[]);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(
                read(&keyless, AccountTier::Demo, kind),
                not_offered_words("oanda", kind).into()
            );
        }
        let why = read(&keyless, AccountTier::Demo, VenueReadKind::OpenOrders);
        assert_eq!(why, OandaVenueReadRefusal::NoPair.carried());
        let paired = AdapterFixture::new(&[("API_KEY", "token"), ("ACCOUNT_ID", "001")]);
        let why = read(&paired, AccountTier::Live, VenueReadKind::OpenOrders);
        assert_eq!(why, OandaVenueReadRefusal::LiveTier.carried());
    }
}
