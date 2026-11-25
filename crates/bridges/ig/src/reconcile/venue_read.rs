//! `venue_read` — an OPERATOR's read of an IG account, asked of IG now: its working orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue open-orders ig` prints it). Nothing is folded and nothing changes at IG.
//!
//! `GET /workingorders` (v2) is ACCOUNT-WIDE, every epic's resting LIMIT and STOP, and it is parsed by the
//! reconcile client's own parser (`crate::reconcile`'s `parse_all_working_orders`), so the two reads
//! cannot disagree about a row. A STOP's `orderLevel` is its trigger, so it shows under `trigger_price` and
//! not under `price` (the resting LIMIT price, as on every other venue's read); `orderType` is the kind.
//!
//! ⚠ **It rides the account's running ORDER session and opens no login of its own.** IG meters
//! `POST /session` and warns that concurrent sessions of one key may get it suspended
//! (`crates/bridges/ig/CLAUDE.md`), so the read borrows the exec thread's session
//! (`crate::account::venue_link`'s `SessionLink`), and an account whose exec client is not running in
//! this daemon is answered in words. The balance and the positions are not offered.

use vike_bridge_core::venue_adapter::AdapterInputs;
use vike_model::{VenueRead, VenueReadKind, VenueReadOpenOrder, not_offered_words};
use vike_utility::errors::CarriedRefusal;

use crate::account::venue_link::session_link;
use crate::reconcile::{VENUE, parse_all_working_orders};
use crate::refusals::IgVenueReadRefusal;

/// The answer to `kind` for the account in `inputs`. `Err` is the operator's sentence, its refusal's id kept.
pub(crate) fn venue_read(
    inputs: &AdapterInputs<'_>,
    kind: VenueReadKind,
) -> Result<VenueRead, CarriedRefusal> {
    if kind != VenueReadKind::OpenOrders {
        return Err(not_offered_words(VENUE, kind).into());
    }
    // An account whose exec client is not running here has no session to borrow.
    let session = session_link(inputs.account)
        .order_session()
        .ok_or_else(|| IgVenueReadRefusal::NotRunning.carried())?;
    let body = session
        .get("/workingorders", "2", "")
        .map_err(|e| IgVenueReadRefusal::ReadFailed { why: e.to_string() }.carried())?;
    let (orders, truncated) = open_orders_from(&body.to_string())?;
    Ok(VenueRead::OpenOrders { orders, truncated })
}

/// A `GET /workingorders` (v2) body → its rows, each under the USER name of the kept row whose epic it rests on (the
/// epic itself, said, when no row names it: `crate::epic`'s `user_name_or_epic`), cut at
/// [`vike_model::MAX_VENUE_OPEN_ORDERS`] with the cut said. IG lists no reference of ours on a working order, so
/// no row names an order client id.
fn open_orders_from(body: &str) -> Result<(Vec<VenueReadOpenOrder>, bool), String> {
    let mut rows = parse_all_working_orders(body, &|_| None)?;
    for row in &mut rows {
        row.symbol = crate::epic::user_name_or_epic(&row.symbol);
    }
    Ok(vike_model::open_orders_read(rows))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vike_bridge_core::venue_adapter::VenueAdapter;
    use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;

    use super::*;
    use crate::adapter::VenueAdapterIg;

    /// Two working orders on two epics, in IG's documented `GET /workingorders` v2 shape (hand-written, not a
    /// capture): a BUY LIMIT on EUR/USD and a SELL STOP on GBP/USD.
    const BODY: &str = r#"{"workingOrders":[
        {"workingOrderData":{"dealId":"DIWO1","epic":"CS.D.EURUSD.MINI.IP","direction":"BUY",
                             "orderType":"LIMIT","orderSize":1.5,"orderLevel":1.05,
                             "timeInForce":"GOOD_TILL_CANCELLED","createdDateUTC":"2022-01-15T14:00:00"},
         "marketData":{"epic":"CS.D.EURUSD.MINI.IP"}},
        {"workingOrderData":{"dealId":"DIWO2","epic":"CS.D.GBPUSD.MINI.IP","direction":"SELL",
                             "orderType":"STOP","orderSize":0.04,"orderLevel":1.2412}}
    ]}"#;

    /// Every epic's row, under the USER name of its kept row (decision 0290): the read is the account's, not the
    /// mount's. An epic no kept row names keeps its own name.
    #[test]
    fn the_read_lists_every_epics_working_order() {
        crate::epic::hold_kept_list_for_tests();
        let (orders, truncated) = open_orders_from(BODY).unwrap();
        assert!(!truncated);
        let (limit, stop) = (&orders[0], &orders[1]);
        assert_eq!((limit.symbol.as_str(), limit.venue_order_id.as_str()), ("EURUSD", "DIWO1"));
        assert_eq!((limit.side, limit.price, limit.qty), (1, Some(1.05), 1.5));
        assert_eq!(
            (limit.status.as_str(), limit.timestamp_ms, limit.filled_qty),
            ("ACCEPTED", 1_642_255_200_000, 0.0)
        );
        assert_eq!((stop.symbol.as_str(), stop.side, stop.price), ("GBPUSD", -1, None));
        let foreign = BODY.replace("CS.D.GBPUSD.MINI.IP", "IX.D.FTSE.CFD.IP");
        let (orders, _) = open_orders_from(&foreign).unwrap();
        assert_eq!(orders[1].symbol, "IX.D.FTSE.CFD.IP", "no kept row: the epic, said at WARN");
        assert_eq!((limit.trigger_price, limit.order_type.as_str()), (None, "limit"));
        assert_eq!(
            (stop.trigger_price, stop.order_type.as_str()),
            (Some(1.2412), "stop"),
            "a STOP lists its level as its trigger"
        );
        assert_eq!(limit.order_client_id, None, "IG lists no reference of ours");
        assert!(open_orders_from("{}").is_err(), "no `workingOrders` array is an error");
    }

    /// **No login is opened for the read.** Without a running order session the account is answered in words
    /// before anything is asked of IG; with one, the read is ONE `GET /workingorders` on it, and no `POST /session`.
    #[test]
    fn the_read_rides_the_running_order_session_and_logs_in_nowhere() {
        let mut fx = AdapterFixture::new(&[]);
        fx.account = AccountLabel::parse("VREADIG").unwrap();
        let read = |kind| VenueAdapterIg.venue_read(&fx.inputs(AccountTier::Demo), kind);
        let not_running = IgVenueReadRefusal::NotRunning.carried();
        assert_eq!(read(VenueReadKind::OpenOrders).unwrap_err(), not_running);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(read(kind).unwrap_err(), not_offered_words("ig", kind).into());
        }

        let (base, seen) = vike_log::loopback::serve_replies("", vec![(200, BODY.to_string())]);
        let session = Arc::new(crate::transport::IgSession::offline(&base));
        session_link(&fx.account).hold_session(&session);
        let Ok(VenueRead::OpenOrders { orders, truncated: false }) =
            read(VenueReadKind::OpenOrders)
        else {
            panic!("the working orders are read through the borrowed session");
        };
        assert_eq!(orders.len(), 2);
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert!(requests[0].starts_with("GET /workingorders "), "{requests:?}");

        drop(session);
        assert_eq!(
            read(VenueReadKind::OpenOrders).unwrap_err(),
            not_running,
            "a stopped client lends nothing"
        );
    }
}
