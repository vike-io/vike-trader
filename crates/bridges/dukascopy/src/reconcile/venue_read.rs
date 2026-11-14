//! `venue_read` — an OPERATOR's read of a Dukascopy account, asked of the venue now: its resting orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue dukascopy open-orders` prints it). Nothing is folded and nothing changes at the venue.
//!
//! The venue has no REST API: its one trading path is the JForex sidecar the account's exec client runs. So the
//! read is the reconcile client's own question (`crate::reconcile::query`'s `ReconcileLink::ask`, answered from
//! `engine.getOrders()`, which spans the account) mapped by its own parser (`crate::reconcile`'s
//! `order_reports`), so the two reads cannot disagree about a row: a plain limit's open price is its PRICE, a
//! resting stop's (and a stop-limit's or an if-touched order's) its TRIGGER, the label's order client id OUR ID, and the
//! node's resting stop-loss or take-profit is a row too, under its order client id (`sl:` / `tp:` and the orders it covers).
//!
//! ⚠ **It rides the account's RUNNING sidecar** (`crate::execution`'s `lent_reconcile_source`) and starts none of its own: a
//! process holds one sidecar, and a second login would take the account's session. An account whose exec client
//! is not running in this daemon is answered in words. The balance and the positions are not offered.

use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{VenueRead, VenueReadKind, not_offered_words};

use crate::reconcile::{ReconcileSource, VENUE, order_reports};

/// The answer to an account whose exec client is not running here: there is no sidecar to ask.
const NOT_RUNNING: &str = "dukascopy: this account's JForex sidecar is not running in this daemon, and this read \
    starts none of its own (a process holds one sidecar); read it while the account runs";

/// The answer to `kind` for `account`. `Err` is the operator's sentence.
pub(crate) fn venue_read(account: &AccountLabel, kind: VenueReadKind) -> Result<VenueRead, String> {
    if kind != VenueReadKind::OpenOrders {
        return Err(not_offered_words(VENUE, kind));
    }
    let source =
        crate::execution::lent_reconcile_source(account).ok_or_else(|| NOT_RUNNING.to_string())?;
    read_over(source.as_ref())
}

/// The read over a source (the running client's, or a test's).
fn read_over(source: &dyn ReconcileSource) -> Result<VenueRead, String> {
    let (orders, truncated) =
        vike_model::open_orders_read(order_reports(&source.ask()?, &source.protections()));
    Ok(VenueRead::OpenOrders { orders, truncated })
}

#[cfg(test)]
mod tests {
    use vike_bridge_core::venue_adapter::VenueAdapter;
    use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
    use vike_model::{AccountTier, OrderRequest, OrderType};

    use super::*;
    use crate::adapter::VenueAdapterDukascopy;
    use crate::reconcile::query::ReconcileAnswer;
    use crate::transport::ReconcileOrder;

    /// The `orders` of a sidecar `recon` answer in `ReconSnapshot.java`'s shape (hand-written, not a capture): our
    /// BUYLIMIT on EURUSD (its label's order client id), a SELLSTOP on GBPUSD opened in the platform, and a FILLED buy on USDJPY
    /// that carries the stop-loss our protection asked for.
    const ORDERS: &str = r#"[
        {"order_id":"11","order_client_id":"S00A307k3x9q2m7d1p4ENLM","symbol":"EURUSD","side":1,"state":"OPENED",
         "command":"BUYLIMIT","amount":0.0,"requested":2000.0,"price":1.05,"created":1700000000000},
        {"order_id":"12","symbol":"GBPUSD","side":-1,"state":"OPENED","command":"SELLSTOP",
         "amount":0.0,"requested":1000.0,"price":1.2412,"created":1700000005000},
        {"order_id":"13","order_client_id":"S00A307k3x9q2m7d1p4ENMK","symbol":"USDJPY","side":1,"state":"FILLED",
         "command":"BUY","amount":1000.0,"requested":1000.0,"price":156.9,"stop_loss":155.0,
         "created":1700000001000,"filled":1700000002000}
    ]"#;

    /// Our resting stop-loss on the USDJPY buy, as the exec client remembers it.
    const STOP_ORDER_CLIENT_ID: &str = "S00A307k3x9q2m7d1p4CLSL";

    struct Answered;

    impl ReconcileSource for Answered {
        fn ask(&self) -> Result<ReconcileAnswer, String> {
            let orders: Vec<ReconcileOrder> =
                serde_json::from_str(ORDERS).map_err(|e| e.to_string())?;
            Ok(ReconcileAnswer {
                orders,
                positions: Vec::new(),
                foreign: 1,
                timestamp: 1_700_000_009_000,
            })
        }
        fn protections(&self) -> Vec<(String, OrderRequest)> {
            let stop = OrderRequest {
                order_client_id: STOP_ORDER_CLIENT_ID.into(),
                venue: VENUE.into(),
                symbol: "USDJPY".into(),
                side: -1,
                qty: 1000.0,
                order_type: OrderType::Stop,
                trigger_price: Some(155.0),
                reduce_only: true,
                ..Default::default()
            };
            vec![(STOP_ORDER_CLIENT_ID.to_string(), stop)]
        }
        fn know(&self, _: &str) {}
        fn forget(&self, _: &str) {}
        fn known_symbols(&self) -> Vec<String> {
            Vec::new()
        }
    }

    struct Down;

    impl ReconcileSource for Down {
        fn ask(&self) -> Result<ReconcileAnswer, String> {
            Err("dukascopy: the JForex sidecar did not answer".into())
        }
        fn protections(&self) -> Vec<(String, OrderRequest)> {
            Vec::new()
        }
        fn know(&self, _: &str) {}
        fn forget(&self, _: &str) {}
        fn known_symbols(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// Every instrument's resting order is a row with its kind, the limit's price under PRICE, the stop's under
    /// TRIGGER and our order client id under OUR ID; the filled buy is a position, not a row, and our stop-loss on it is one.
    #[test]
    fn every_resting_order_and_our_protection_is_listed_with_its_kind_trigger_and_our_id() {
        let Ok(VenueRead::OpenOrders { orders, truncated: false }) = read_over(&Answered) else {
            panic!("the sidecar's answer is read");
        };
        assert_eq!(orders.len(), 3, "{orders:?}");
        let by_id = |id: &str| orders.iter().find(|o| o.venue_order_id == id).expect(id);
        let limit = by_id("11");
        assert_eq!((limit.symbol.as_str(), limit.side, limit.qty), ("EURUSD", 1, 2000.0));
        assert_eq!(
            (limit.price, limit.trigger_price, limit.order_type.as_str()),
            (Some(1.05), None, "limit")
        );
        assert_eq!(limit.order_client_id.as_deref(), Some("S00A307k3x9q2m7d1p4ENLM"));
        assert_eq!((limit.status.as_str(), limit.timestamp_ms), ("ACCEPTED", 1_700_000_000_000));
        let stop = by_id("12");
        assert_eq!((stop.symbol.as_str(), stop.side), ("GBPUSD", -1));
        assert_eq!(
            (stop.price, stop.trigger_price, stop.order_type.as_str()),
            (None, Some(1.2412), "stop"),
            "a resting stop's open price is its TRIGGER"
        );
        assert_eq!(stop.order_client_id, None, "a platform order names no order client id");
        let protection = by_id("sl:13");
        assert_eq!(protection.order_client_id.as_deref(), Some(STOP_ORDER_CLIENT_ID));
        assert_eq!((protection.symbol.as_str(), protection.side), ("USDJPY", -1));
        assert_eq!(
            (protection.trigger_price, protection.order_type.as_str()),
            (Some(155.0), "stop")
        );
        assert!(orders.iter().all(|o| o.venue_order_id != "13"), "a filled order is a position");
    }

    /// A question the sidecar could not answer is the error, never an empty list; an account whose sidecar does not run
    /// here is told so, and a kind this bridge does not read is not offered.
    #[test]
    fn a_read_that_cannot_be_made_is_refused_in_words() {
        assert!(read_over(&Down).unwrap_err().contains("did not answer"));
        let fx = AdapterFixture::new(&[]);
        let read = |kind| VenueAdapterDukascopy.venue_read(&fx.inputs(AccountTier::Demo), kind);
        assert_eq!(read(VenueReadKind::OpenOrders).unwrap_err(), NOT_RUNNING);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(read(kind).unwrap_err(), not_offered_words("dukascopy", kind));
        }
    }
}
