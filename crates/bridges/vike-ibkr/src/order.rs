//! Order mapping: vike `OrderRequest` → an `IbOrderSpec` (the plain field set the socket backend
//! hands to `ibapi::Order`), plus the typed IB order-status enum with `is_terminal()`/`is_active()`
//! that drives inflight-map cleanup in the event mapper (Task 7). Pure + fixture-tested; no I/O.

use crate::contract::IbkrContract;
use vike_model::OrderRequest;

/// IB order status, typed (replaces IB's magic status strings). `is_terminal()` drives coid⇄orderId
/// map removal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderStatusKind {
    ApiPending,
    PendingSubmit,
    PendingCancel,
    PreSubmitted,
    Submitted,
    ApiCancelled,
    Cancelled,
    Filled,
    Inactive,
}

impl OrderStatusKind {
    pub fn from_ib(s: &str) -> OrderStatusKind {
        match s {
            "ApiPending" => OrderStatusKind::ApiPending,
            "PendingSubmit" => OrderStatusKind::PendingSubmit,
            "PendingCancel" => OrderStatusKind::PendingCancel,
            "PreSubmitted" => OrderStatusKind::PreSubmitted,
            "Submitted" => OrderStatusKind::Submitted,
            "ApiCancelled" | "ApiCanceled" => OrderStatusKind::ApiCancelled,
            "Cancelled" | "Canceled" => OrderStatusKind::Cancelled,
            "Filled" => OrderStatusKind::Filled,
            _ => OrderStatusKind::Inactive,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatusKind::Filled | OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled
        )
    }
    pub fn is_active(self) -> bool {
        matches!(
            self,
            OrderStatusKind::PreSubmitted
                | OrderStatusKind::Submitted
                | OrderStatusKind::PendingSubmit
        )
    }
}

/// The plain field set the socket backend copies onto an `ibapi::Order`. Keeping it a pure struct
/// makes order construction fixture-testable without the ibapi types.
#[derive(Clone, Debug, PartialEq)]
pub struct IbOrderSpec {
    pub action: &'static str, // "BUY" | "SELL"
    pub total_qty: f64,
    pub order_type: &'static str, // "MKT" | "LMT" | "STP"
    pub lmt_price: Option<f64>,
    pub aux_price: Option<f64>, // stop trigger
    pub tif: &'static str,      // "GTC" | "DAY" | "IOC" | "FOK" | "GTD"
    pub order_ref: String,      // = client_order_id (round-trips as the coid tag)
}

/// This venue's row of the ONE cross-venue TIF authority ([`vike_bridge_core::tif::venue_tif`]),
/// consumed — all five TIFs map 1:1 to native IB strings; a future TIF flip is a one-line row
/// edit there. CAVEAT (pinned in the table row too): GTD is mapped but INOPERABLE today —
/// neither backend wires the required good-till date from `req.gtd_expiry` (the socket backend's
/// `build_order` keeps `Order::default`'s empty `good_till_date`; the cpapi order body carries
/// no date field), and TWS/CPAPI reject a GTD order lacking its date.
fn tif_of(req: &OrderRequest) -> &'static str {
    vike_bridge_core::tif::venue_tif("ibkr", req.time_in_force).wire().unwrap_or("GTC")
}

/// `price_magnifier` (bonds) divides the display price into IB wire units; 1 for everything else.
fn apply_magnifier(px: f64, price_magnifier: i32) -> f64 {
    if price_magnifier > 1 { px / price_magnifier as f64 } else { px }
}

pub fn map_order_request(
    req: &OrderRequest,
    _contract: &IbkrContract,
    price_magnifier: i32,
) -> IbOrderSpec {
    let action = if req.side >= 0 { "BUY" } else { "SELL" };
    let (order_type, lmt_price, aux_price) = match req.order_type.as_str() {
        "limit" => ("LMT", req.price.map(|p| apply_magnifier(p, price_magnifier)), None),
        "stop" => ("STP", None, req.trigger_price.map(|p| apply_magnifier(p, price_magnifier))),
        _ => ("MKT", None, None),
    };
    IbOrderSpec {
        action,
        total_qty: req.qty,
        order_type,
        lmt_price,
        aux_price,
        tif: tif_of(req),
        order_ref: req.client_order_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_simplified;
    use vike_model::OrderRequest;

    fn req(side: i32, ty: &str, price: Option<f64>) -> OrderRequest {
        OrderRequest {
            client_order_id: "coid-1".into(),
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            side,
            qty: 10.0,
            order_type: ty.into(),
            price,
            trigger_price: None,
            ..Default::default()
        }
    }

    #[test]
    fn market_buy_maps() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let spec = map_order_request(&req(1, "market", None), &c, 1);
        assert_eq!(spec.action, "BUY");
        assert_eq!(spec.order_type, "MKT");
        assert_eq!(spec.total_qty, 10.0);
        assert_eq!(spec.order_ref, "coid-1"); // client_order_id rides IB orderRef
        assert!(spec.lmt_price.is_none());
    }

    #[test]
    fn limit_sell_maps_price() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let spec = map_order_request(&req(-1, "limit", Some(191.25)), &c, 1);
        assert_eq!(spec.action, "SELL");
        assert_eq!(spec.order_type, "LMT");
        assert_eq!(spec.lmt_price, Some(191.25));
    }

    #[test]
    fn stop_maps_aux_price() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let mut r = req(-1, "stop", None);
        r.trigger_price = Some(180.0);
        let spec = map_order_request(&r, &c, 1);
        assert_eq!(spec.order_type, "STP");
        assert_eq!(spec.aux_price, Some(180.0));
    }

    /// Equivalence gate for the `venue_tif` routing: the five recorded IB TIF strings, asserted
    /// BOTH through the mapped spec and against this venue's row of the cross-venue table
    /// (byte-for-byte). NOTE the GTD stub: the string maps 1:1 but neither backend wires the
    /// required good-till date from `gtd_expiry` (socket `build_order` keeps `Order::default`'s
    /// empty `good_till_date`; the cpapi body carries no date field) — pinned in the table row.
    #[test]
    fn tif_matches_the_venue_tif_row() {
        use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        for (tif, want) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "GTD"), (Day, "DAY")] {
            let mut r = req(1, "limit", Some(100.0));
            r.time_in_force = tif;
            assert_eq!(map_order_request(&r, &c, 1).tif, want, "{tif:?}");
            assert_eq!(
                vike_bridge_core::tif::venue_tif("ibkr", tif).wire(),
                Some(want),
                "table row {tif:?}"
            );
        }
    }

    #[test]
    fn status_terminality() {
        assert!(OrderStatusKind::Filled.is_terminal());
        assert!(OrderStatusKind::Cancelled.is_terminal());
        assert!(OrderStatusKind::ApiCancelled.is_terminal());
        assert!(OrderStatusKind::Submitted.is_active());
        assert!(!OrderStatusKind::Submitted.is_terminal());
        assert_eq!(OrderStatusKind::from_ib("PreSubmitted"), OrderStatusKind::PreSubmitted);
    }
}
