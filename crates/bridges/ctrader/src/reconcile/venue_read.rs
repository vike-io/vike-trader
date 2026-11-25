//! `venue_read` — an OPERATOR's read of a cTrader account, asked of the venue now: its pending orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue open-orders ctrader` prints it). Nothing is folded and nothing changes at the venue.
//!
//! `ProtoOAReconcileReq` is ACCOUNT-WIDE (it takes no symbol and lists every pending order of the trading
//! account), and its rows go through the reconcile client's own parser (`crate::reconcile`'s
//! `parse_orders`, via `ReconcileClientCtrader::all_open_orders`), so the two reads cannot disagree about a row:
//! a LIMIT's `limitPrice` is its price, a STOP's `stopPrice` its trigger, a STOP_LIMIT states both, and our
//! order client id is the order's `clientOrderId` (the node writes it there) or a label that reads as one of our ids.
//!
//! ⚠ **It opens ONE short-lived authed socket of its own**, the reconcile client's handshake, as the startup
//! credential probe does (scoped to the symbol the account's row states, which the handshake resolves in the
//! account's own list: `crate::adapter`'s `account_symbol_of`): joined to the process's shared grant
//! (`ConnectionConfig::sync_grant`), so it FOLLOWS a rotation the running sockets made and never spends the refresh
//! token. Only the DEMO host is dialled; a `live` account is told so with nothing sent. The balance and the
//! positions are not offered.

use vike_model::VenueRead;
use vike_utility::errors::CarriedRefusal;

use crate::config::CtraderConfig;
use crate::reconcile::ReconcileClientCtrader;
use crate::refusals::CtraderVenueReadRefusal;

/// The account's pending orders, over one socket built from `config` (its handshake resolving `symbol`, the one the
/// account's row states) and dropped after the one request.
pub(crate) fn open_orders(
    config: &CtraderConfig,
    symbol: &str,
) -> Result<VenueRead, CarriedRefusal> {
    let client = ReconcileClientCtrader::connect(&config.to_connection_config(), symbol)
        .ok_or_else(|| {
            CtraderVenueReadRefusal::ConnectRefused { host: config.host.clone(), port: config.port }
                .carried()
        })?;
    let (orders, truncated) = vike_model::open_orders_read(client.all_open_orders()?);
    Ok(VenueRead::OpenOrders { orders, truncated })
}

#[cfg(test)]
mod tests {
    use vike_bridge_core::venue_adapter::VenueAdapter;
    use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
    use vike_model::{
        AccountTier, MAX_VENUE_OPEN_ORDERS, VenueReadKind, not_offered_words, open_orders_read,
    };

    use crate::adapter::VenueAdapterCtrader;
    use crate::instrument::symbols::SymbolMap;
    use crate::reconcile::parse_orders;
    use crate::refusals::CtraderVenueReadRefusal;
    use crate::transport::proto::{
        ProtoOaLightSymbol, ProtoOaOrder, ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaSymbol,
        ProtoOaTradeData, ProtoOaTradeSide,
    };

    /// One of our v2 ids, as the node writes it into `clientOrderId`.
    const OURS: &str = "S00A307k3x9q2m7d1p4CLSL";

    fn symbols() -> SymbolMap {
        let light = |symbol_id, name: &str| ProtoOaLightSymbol {
            symbol_id,
            symbol_name: Some(name.to_string()),
            enabled: Some(true),
            ..Default::default()
        };
        let full = |symbol_id| ProtoOaSymbol {
            symbol_id,
            digits: 5,
            pip_position: 4,
            ..Default::default()
        };
        SymbolMap::from_symbols(&[light(1, "EURUSD"), light(2, "GBPUSD")], &[full(1), full(2)])
    }

    fn order(
        id: i64,
        symbol_id: i64,
        side: ProtoOaTradeSide,
        kind: ProtoOaOrderType,
    ) -> ProtoOaOrder {
        ProtoOaOrder {
            order_id: id,
            trade_data: ProtoOaTradeData {
                symbol_id,
                volume: 150_000,
                trade_side: side as i32,
                ..Default::default()
            },
            order_type: kind as i32,
            order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
            utc_last_update_timestamp: Some(1_700_000_000_000),
            ..Default::default()
        }
    }

    /// A `ProtoOAReconcileRes.order` list of two symbols (constructed prost rows, the venue's documented fields;
    /// not a capture): our BUY LIMIT on EURUSD and a SELL STOP on GBPUSD placed in the platform. Every symbol's
    /// row is listed under the broker's name, the limit's price under PRICE, the stop's under TRIGGER, our order client id
    /// under OUR ID and no id for the foreign one.
    #[test]
    fn every_symbols_pending_order_is_listed_with_its_kind_trigger_and_our_id() {
        let limit = ProtoOaOrder {
            client_order_id: Some(OURS.to_string()),
            limit_price: Some(1.05),
            ..order(42, 1, ProtoOaTradeSide::Buy, ProtoOaOrderType::Limit)
        };
        let mut stop = order(43, 2, ProtoOaTradeSide::Sell, ProtoOaOrderType::Stop);
        stop.stop_price = Some(1.2412);
        stop.trade_data.label = Some("my hedge".to_string());
        let (orders, truncated) = open_orders_read(parse_orders(&[limit, stop], &symbols(), None));
        assert!(!truncated);
        let (l, s) = (&orders[0], &orders[1]);
        assert_eq!((l.symbol.as_str(), l.venue_order_id.as_str(), l.side), ("EURUSD", "42", 1));
        assert_eq!((l.price, l.trigger_price, l.order_type.as_str()), (Some(1.05), None, "limit"));
        assert_eq!(l.order_client_id.as_deref(), Some(OURS), "our order client id is under OUR ID");
        assert_eq!((l.qty, l.filled_qty, l.status.as_str()), (1500.0, 0.0, "ACCEPTED"));
        assert_eq!(l.timestamp_ms, 1_700_000_000_000);
        assert_eq!((s.symbol.as_str(), s.venue_order_id.as_str(), s.side), ("GBPUSD", "43", -1));
        assert_eq!(
            (s.price, s.trigger_price, s.order_type.as_str()),
            (None, Some(1.2412), "stop"),
            "a STOP's stopPrice is its TRIGGER"
        );
        assert_eq!(s.order_client_id, None, "a platform order's label is no id of ours");
    }

    #[test]
    fn rows_past_the_cap_are_cut_and_said_to_be() {
        let rows: Vec<ProtoOaOrder> = (0..=MAX_VENUE_OPEN_ORDERS as i64)
            .map(|i| order(i, 1, ProtoOaTradeSide::Buy, ProtoOaOrderType::Limit))
            .collect();
        let (orders, truncated) = open_orders_read(parse_orders(&rows, &symbols(), None));
        assert_eq!((orders.len(), truncated), (MAX_VENUE_OPEN_ORDERS, true));
        assert_eq!(
            open_orders_read(Vec::new()),
            (Vec::new(), false),
            "nothing pending is an answer"
        );
    }

    /// Refused in words before any socket: a kind this bridge does not read, an account with no grant, and a
    /// `live` account holding one (this arm dials only the demo host).
    #[test]
    fn a_read_that_cannot_be_made_is_refused_before_any_socket() {
        let read = |fx: &AdapterFixture, tier, kind| {
            VenueAdapterCtrader.venue_read(&fx.inputs(tier), kind).unwrap_err()
        };
        let keyless = AdapterFixture::new(&[]);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(
                read(&keyless, AccountTier::Demo, kind),
                not_offered_words("ctrader", kind).into()
            );
        }
        let why = read(&keyless, AccountTier::Demo, VenueReadKind::OpenOrders);
        assert_eq!(why, CtraderVenueReadRefusal::NoGrant.carried());

        let granted =
            AdapterFixture::new(&[("ACCESS_TOKEN", "access"), ("REFRESH_TOKEN", "refresh")])
                .with_venue_fields(&[("CLIENT_ID", "app-id"), ("CLIENT_SECRET", "app-secret")]);
        let why = read(&granted, AccountTier::Live, VenueReadKind::OpenOrders);
        assert_eq!(why, CtraderVenueReadRefusal::LiveTier.carried());

        // A demo grant with its addresses whose row states no symbol: the handshake resolves one in the account's
        // own list, so the read is refused naming the row (owner, 2026-11-13: never a compiled symbol), before any
        // socket.
        let mut unstated =
            AdapterFixture::new(&[("ACCESS_TOKEN", "access"), ("REFRESH_TOKEN", "refresh")])
                .with_venue_fields(&[("CLIENT_ID", "app-id"), ("CLIENT_SECRET", "app-secret")]);
        unstated.settings = vike_secrets::venue_setting::VenueSettings::default()
            .with_urls(vike_venues::venue_urls::VenueUrls::seed("ctrader"));
        let why = read(&unstated, AccountTier::Demo, VenueReadKind::OpenOrders);
        assert_eq!(why, crate::refusals::CtraderStartRefusal::NoAccountSymbol.carried());
    }
}
