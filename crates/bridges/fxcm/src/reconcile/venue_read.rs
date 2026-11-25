//! `venue_read` — an OPERATOR's read of an FXCM account, asked of the venue now: its resting orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue open-orders fxcm` prints it). Nothing is folded and nothing changes at the venue.
//!
//! The Orders table (`forex_connect_orders`, a fresh refresh-table request for the tradable account) is a whole-account
//! snapshot, read the way the reconcile pass reads it and parsed by its own parser (`crate::reconcile`'s
//! `parse_orders_with`, once per instrument the table names), so the two reads cannot disagree about a row: the order
//! code is the KIND (`LE`/`L` limit, `SE`/`S`/`STE` stop, any other code lower-cased), `buysell` the side,
//! `amount` the size, and OUR ID the order client id this process routes the order id under (`crate::execution::registry`; an order
//! placed elsewhere, or before a restart nothing re-taught, names none). Each row is named by its instrument's USER
//! name (`EURUSD` for the venue's `EUR/USD`, decision 0290), or by the venue's own name where no kept row names it.
//!
//! ⚠ **No PRICE and no TRIGGER, on any row.** The shim's `forex_connect_orders` row carries no rate (its fields are the order
//! id, the instrument, the side, the amount, the type and the status), so a limit's price and a stop's trigger
//! cannot be stated; both print `-`. Stating them is a shim change (`IO2GOrderRow::getRate` into that row).
//!
//! ⚠ **It rides the account's RUNNING reconcile session** (`crate::account::reads`, where that session registers its request
//! port) and opens no login of its own: FXCM sessions are scarce. An account with no reconcile session in this
//! daemon (reconciliation off, or its login has not opened) is answered in words. The balance and the positions are
//! not offered.

use serde_json::Value;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{VenueRead, VenueReadKind, VenueReadOpenOrder, not_offered_words};
use vike_utility::errors::CarriedRefusal;

use crate::execution::registry::OrderClientIdLookup;
use crate::reconcile::{VENUE, booked_name, parse_orders_with};
use crate::refusals::FxcmVenueReadRefusal;

/// The answer to `kind` for `account`. `Err` is the operator's sentence, its refusal's id kept.
pub(crate) fn venue_read(
    account: &AccountLabel,
    kind: VenueReadKind,
) -> Result<VenueRead, CarriedRefusal> {
    if kind != VenueReadKind::OpenOrders {
        return Err(not_offered_words(VENUE, kind).into());
    }
    let reads = crate::account::reads::account_reads(account.text().unwrap_or_default())
        .ok_or_else(|| FxcmVenueReadRefusal::NotRunning.carried())?;
    let body = reads
        .orders_json()
        .map_err(|error| FxcmVenueReadRefusal::OrdersUnread { error }.carried())?;
    let (orders, truncated) = open_orders_from(&body, &crate::execution::registry::process())
        .map_err(|why| why.carried())?;
    Ok(VenueRead::OpenOrders { orders, truncated })
}

/// An `forex_connect_orders` body → its rows, each under the name the reconcile pass reports it under
/// (`crate::reconcile`'s `booked_name`: its instrument's USER name, decision 0290, else the venue's own), cut at
/// [`vike_model::MAX_VENUE_OPEN_ORDERS`].
fn open_orders_from(
    body: &str,
    ours: &OrderClientIdLookup,
) -> Result<(Vec<VenueReadOpenOrder>, bool), FxcmVenueReadRefusal> {
    let rows: Value = serde_json::from_str(body)
        .map_err(|e| FxcmVenueReadRefusal::OrdersUnparsed { error: e.to_string() })?;
    let rows = rows.as_array().ok_or(FxcmVenueReadRefusal::OrdersNotList)?;
    // each name once
    let mut names: Vec<String> = Vec::new();
    for instrument in rows.iter().filter_map(|o| o.get("instrument").and_then(Value::as_str)) {
        let name = booked_name(instrument);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut reports = Vec::new();
    for name in &names {
        reports.extend(
            parse_orders_with(body, name, ours)
                .map_err(|error| FxcmVenueReadRefusal::OrdersUnparsed { error })?,
        );
    }
    Ok(vike_model::open_orders_read(reports))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vike_bridge_core::venue_adapter::VenueAdapter;
    use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
    use vike_model::AccountTier;

    use super::*;
    use crate::account::quote_rate::PropertiesRead;
    use crate::account::reads::SessionReads;
    use crate::adapter::VenueAdapterFxcm;

    /// Our order client id, which the exec thread bound to the venue order id of our resting limit.
    const OURS: &str = "S00A307k3x9q2m7d1p4ENLM";

    /// An Orders table as `forex_connect_orders` writes it (the shape `execution/execution_tests.rs`'s `order_row` pins; hand-written, not a
    /// capture): our resting BUY limit entry of 1000 EUR/USD, and a SELL stop entry of 2000 GBP/USD placed in the
    /// platform.
    const BODY: &str = r#"[
        {"order_id":"416154901","instrument":"EUR/USD","buysell":"B","amount":1000,"type":"LE","status":"W"},
        {"order_id":"416154902","instrument":"GBP/USD","buysell":"S","amount":2000,"type":"SE","status":"W"}
    ]"#;

    /// A reconcile session that answers the Orders table with [`BODY`].
    struct Session;

    impl SessionReads for Session {
        fn system_properties(&self) -> PropertiesRead {
            PropertiesRead::Failed("not asked".into())
        }
        fn offers(&self) -> Result<String, String> {
            Err("not asked".into())
        }
        fn orders(&self) -> Result<String, String> {
            Ok(BODY.to_string())
        }
    }

    /// Every pair's resting order is a row under its instrument's USER name (decision 0290), with its KIND, side and
    /// size; our order names our order client id and the platform's names none; no row states a PRICE or a TRIGGER
    /// (the shim's row has no rate). An instrument no kept row names keeps the venue's own name.
    #[test]
    fn every_pairs_resting_order_is_listed_with_its_kind_side_size_and_our_id() {
        let mut ours = OrderClientIdLookup::default();
        ours.bind("416154901", OURS);
        let (orders, truncated) = open_orders_from(BODY, &ours).unwrap();
        assert!(!truncated);
        let (limit, stop) = (&orders[0], &orders[1]);
        assert_eq!((limit.symbol.as_str(), limit.venue_order_id.as_str()), ("EURUSD", "416154901"));
        assert_eq!((limit.side, limit.qty, limit.order_type.as_str()), (1, 1000.0, "limit"));
        assert_eq!(
            limit.order_client_id.as_deref(),
            Some(OURS),
            "our order client id is under OUR ID"
        );
        assert_eq!(
            (limit.status.as_str(), limit.filled_qty, limit.timestamp_ms),
            ("ACCEPTED", 0.0, 0)
        );
        assert_eq!((stop.symbol.as_str(), stop.side, stop.qty), ("GBPUSD", -1, 2000.0));
        assert_eq!(stop.order_type, "stop");
        assert_eq!(stop.order_client_id, None, "a platform order names no order client id");
        for row in &orders {
            assert_eq!((row.price, row.trigger_price), (None, None), "the shim's row has no rate");
        }
        let unlisted = r#"[{"order_id":"9","instrument":"ZZZ/QQQ","buysell":"B","amount":1,"type":"LE","status":"W"}]"#;
        let (orders, _) = open_orders_from(unlisted, &ours).unwrap();
        assert_eq!(orders[0].symbol, "ZZZ/QQQ", "no kept row: the venue's own name, never dropped");
        assert_eq!(open_orders_from("[]", &ours).unwrap(), (Vec::new(), false));
        assert_eq!(open_orders_from(r#"{"x":1}"#, &ours), Err(FxcmVenueReadRefusal::OrdersNotList));
    }

    /// The read asks the account's REGISTERED reconcile session and opens nothing: without one it is told so; with
    /// one, the Orders table it answers is the list, our order client id named from the process's table.
    #[test]
    fn the_read_rides_the_registered_reconcile_session() {
        let mut fx = AdapterFixture::new(&[]);
        fx.account = AccountLabel::parse("VREADFX").unwrap();
        let read = |kind| VenueAdapterFxcm.venue_read(&fx.inputs(AccountTier::Demo), kind);
        let not_running = FxcmVenueReadRefusal::NotRunning.carried();
        assert_eq!(read(VenueReadKind::OpenOrders).unwrap_err(), not_running);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(read(kind).unwrap_err(), not_offered_words("fxcm", kind).into());
        }

        crate::execution::registry::process().bind("416154901", OURS);
        let entry = crate::account::reads::register("VREADFX", Arc::new(Session));
        let Ok(VenueRead::OpenOrders { orders, truncated: false }) =
            read(VenueReadKind::OpenOrders)
        else {
            panic!("the Orders table is read through the registered session");
        };
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].order_client_id.as_deref(), Some(OURS));
        crate::account::reads::unregister("VREADFX", &entry);
        assert_eq!(
            read(VenueReadKind::OpenOrders).unwrap_err(),
            not_running,
            "a session gone lends nothing"
        );
    }
}
