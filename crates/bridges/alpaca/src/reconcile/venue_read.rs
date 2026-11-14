//! `venue_read` — an OPERATOR's read of an Alpaca account, asked of the venue now: its open orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue alpaca open-orders` prints it). Nothing is folded and nothing changes at the venue.
//!
//! `GET /v1/trading/accounts/{id}/orders?status=open` with no `symbols=` filter is ACCOUNT-WIDE (every open
//! order of every symbol), asked on a Bearer transport of its own (the crate's one-lifecycle-per-seam rule,
//! `crates/bridges/alpaca/CLAUDE.md`) and parsed by the reconcile client's own parser (`crate::reconcile`'s
//! `parse_orders`, once per symbol the body names), so the two reads cannot disagree about a row: a limit's
//! `limit_price` is its price, a stop's `stop_price` its TRIGGER, a stop-limit states both. The CLIENT id is
//! the row's `client_order_id`: ours on an order the node placed, and the id the venue mints for one placed
//! anywhere else.
//!
//! One page of `limit` [`ORDER_PAGE_LIMIT`] (the documented maximum), newest first: a full page may not be the
//! whole list and says so (`truncated`). Only the sandbox is asked: a `live` account is told so with nothing
//! sent. The balance and the positions are not offered.

use vike_model::{VenueRead, VenueReadOpenOrder};

use crate::config::AlpacaConfig;
use crate::reconcile::{ORDER_PAGE_LIMIT, parse_orders};
use crate::transport::AlpacaTransport;

/// The account's open orders, on a fresh OAuth2 lifecycle of `config`'s.
pub(crate) fn open_orders(config: &AlpacaConfig) -> Result<VenueRead, String> {
    let token = std::sync::Arc::new(crate::transport::sign::TokenSource::new(
        config.client_id.expose().clone(),
        config.client_secret.expose().clone(),
        config.hosts.authx.clone(),
    ));
    read(&AlpacaTransport::new(token), &config.hosts.broker, &config.account_id)
}

/// The read over `rest`: the seam the offline test drives.
fn read(rest: &AlpacaTransport, broker: &str, account_id: &str) -> Result<VenueRead, String> {
    let path = format!("/v1/trading/accounts/{account_id}/orders");
    let query = vike_bridge_core::urlencode(&[
        ("status", "open".to_string()),
        ("limit", ORDER_PAGE_LIMIT.to_string()),
        ("direction", "desc".to_string()),
    ]);
    let body = rest
        .get(broker, &path, &query)
        .map_err(|e| format!("alpaca: the open orders could not be read: {e}"))?;
    let (orders, truncated) = open_orders_from(&body.to_string())?;
    Ok(VenueRead::OpenOrders { orders, truncated })
}

/// One `orders` page → its rows, each under the symbol the venue names (`AAPL`, `BTC/USD`), cut at
/// [`vike_model::MAX_VENUE_OPEN_ORDERS`]; a full page is said to be cut (the module doc).
fn open_orders_from(body: &str) -> Result<(Vec<VenueReadOpenOrder>, bool), String> {
    let rows: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = rows.as_array().ok_or("alpaca's orders answer is not a list")?;
    let mut symbols: Vec<&str> = Vec::new();
    for symbol in rows.iter().filter_map(|o| o.get("symbol").and_then(|s| s.as_str())) {
        if !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
    }
    let mut reports = Vec::new();
    for symbol in &symbols {
        reports.extend(parse_orders(body, symbol, symbol)?);
    }
    let (orders, cut) = vike_model::open_orders_read(reports);
    // A full page may hide more: the venue pages this list, the shared cap does not know it.
    Ok((orders, rows.len() >= ORDER_PAGE_LIMIT || cut))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vike_bridge_core::venue_adapter::VenueAdapter;
    use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
    use vike_model::{AccountTier, MAX_VENUE_OPEN_ORDERS, VenueReadKind, not_offered_words};

    use super::*;
    use crate::adapter::VenueAdapterAlpaca;
    use crate::transport::sign::TokenSource;

    /// Two symbols' open orders in the Broker API's documented order shape (hand-written, not a capture): our
    /// BUY LIMIT on AAPL and a SELL STOP on BTC/USD placed elsewhere (the venue minted its client id).
    const BODY: &str = r#"[
        {"id":"61e69015-8549-4bfd-b9c3-01e75843f47d","client_order_id":"S00A307k3x9q2m7d1p4ENLM",
         "symbol":"AAPL","side":"buy","order_type":"limit","type":"limit","qty":"5","filled_qty":"0",
         "limit_price":"150.25","stop_price":null,"status":"new","updated_at":"2023-11-14T22:13:20Z"},
        {"id":"0b3e2b9f-0c63-4d6e-9a5d-6c2f1f9e7a11","client_order_id":"eb9e2aaa-f71a-4f51-b5b4-52a6c565dad4",
         "symbol":"BTC/USD","side":"sell","order_type":"stop","type":"stop","qty":"0.04","filled_qty":"0",
         "limit_price":null,"stop_price":"61000","status":"accepted","updated_at":"2023-11-14T22:13:25Z"}
    ]"#;

    #[test]
    fn every_symbols_open_order_is_listed_with_its_kind_trigger_and_client_id() {
        let (orders, truncated) = open_orders_from(BODY).unwrap();
        assert!(!truncated);
        let (limit, stop) = (&orders[0], &orders[1]);
        assert_eq!((limit.symbol.as_str(), limit.side, limit.qty), ("AAPL", 1, 5.0));
        assert_eq!(
            (limit.price, limit.trigger_price, limit.order_type.as_str()),
            (Some(150.25), None, "limit")
        );
        assert_eq!(limit.order_client_id.as_deref(), Some("S00A307k3x9q2m7d1p4ENLM"));
        assert_eq!((limit.status.as_str(), limit.timestamp_ms), ("ACCEPTED", 1_700_000_000_000));
        assert_eq!(limit.venue_order_id, "61e69015-8549-4bfd-b9c3-01e75843f47d");
        assert_eq!((stop.symbol.as_str(), stop.side, stop.qty), ("BTC/USD", -1, 0.04));
        assert_eq!(
            (stop.price, stop.trigger_price, stop.order_type.as_str()),
            (None, Some(61000.0), "stop"),
            "a stop's stop_price is its TRIGGER"
        );
    }

    #[test]
    fn a_full_page_is_said_to_be_cut_and_a_non_list_is_an_error() {
        let row = r#"{"id":"x","symbol":"AAPL","side":"buy","order_type":"limit","qty":"1","status":"new"}"#;
        let full = format!("[{}]", vec![row; ORDER_PAGE_LIMIT].join(","));
        let (orders, truncated) = open_orders_from(&full).unwrap();
        assert_eq!((orders.len(), truncated), (ORDER_PAGE_LIMIT.min(MAX_VENUE_OPEN_ORDERS), true));
        assert_eq!(
            open_orders_from("[]").unwrap(),
            (Vec::new(), false),
            "nothing open is an answer"
        );
        assert!(open_orders_from(r#"{"message":"x"}"#).is_err());
    }

    /// The read is ONE `GET …/orders?status=open` of the account, with no symbol filter.
    #[test]
    fn the_read_is_one_account_wide_open_orders_get() {
        let (base, seen) = vike_log::loopback::serve_replies("", vec![(200, BODY.to_string())]);
        let rest = AlpacaTransport::new(Arc::new(TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Ok(("token".to_string(), 900))),
        )));
        let Ok(VenueRead::OpenOrders { orders, truncated: false }) = read(&rest, &base, "ACC-1")
        else {
            panic!("the open orders are read");
        };
        assert_eq!(orders.len(), 2);
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "{requests:?}");
        let line = &requests[0];
        assert!(line.starts_with("GET /v1/trading/accounts/ACC-1/orders?"), "{line}");
        assert!(line.contains("status=open") && !line.contains("symbols="), "{line}");
    }

    /// Refused in words before any request: a kind this bridge does not read, an account with no trio, and a
    /// `live` account holding one (this arm reaches only the sandbox).
    #[test]
    fn a_read_that_cannot_be_made_is_refused_before_any_request() {
        let read = |fx: &AdapterFixture, tier, kind| {
            VenueAdapterAlpaca.venue_read(&fx.inputs(tier), kind).unwrap_err()
        };
        let keyless = AdapterFixture::new(&[]);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(read(&keyless, AccountTier::Demo, kind), not_offered_words("alpaca", kind));
        }
        let why = read(&keyless, AccountTier::Demo, VenueReadKind::OpenOrders);
        assert!(why.contains("holds no alpaca CLIENT_ID"), "{why}");
        let trio = AdapterFixture::new(&[
            ("CLIENT_ID", "id"),
            ("CLIENT_SECRET", "secret"),
            ("ACCOUNT_ID", "acc"),
        ]);
        let why = read(&trio, AccountTier::Live, VenueReadKind::OpenOrders);
        assert!(why.contains("reaches only Alpaca's sandbox"), "{why}");
    }
}
