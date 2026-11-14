//! `WireIds` and `ResolveOrders`: the two QUERY-ONLY request frames of a lease.
//!
//! Neither places, cancels or amends anything. Both run on the account's dispatch thread (the one that
//! owns the `ExecutionClient`), in order with the commands before them, and answer through the lease that
//! asked.
//!
//! * **`WireIds{order_client_ids}`** is the boot prefetch of restored order client ids: for every order client id whose wire id
//!   (`ExecutionClient::wire_id_for`) is a pure function of the order client id and NOT the order client id itself (hyperliquid's
//!   `cloid = keccak(order client id)`), a `(order client id, wire id)` pair. An order client id the venue echoes verbatim is left out.
//! * **`ResolveOrders{order_client_ids}`** is the query-only resolution of Written-but-unacknowledged submits after
//!   an epoch change (B.7 of the wave-2 plan). Per order client id the daemon says what it did: the order is one it
//!   TRACKS (its events flow, or already flowed), it launched the venue's re-query by order client id
//!   (`confirm`), or the venue has no such lookup and the order stays UNKNOWN until reconcile. It never
//!   sends anything a second time and never invents an event.

use vike_gatewayhub_client::proto::{OrderResolution, ResolveState};

/// What to do about one order client id, from the two facts the daemon has.
pub fn resolution_of(tracked: bool, venue_resolves_by_order_client_id: bool) -> ResolveState {
    if tracked {
        ResolveState::Tracked
    } else if venue_resolves_by_order_client_id {
        ResolveState::Querying
    } else {
        ResolveState::NoResolver
    }
}

/// The `(order client id, wire id)` pairs of `order_client_ids` whose wire id differs from the order client id, in the caller's order.
pub fn wire_pairs(
    order_client_ids: &[String],
    wire_id_for: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    order_client_ids
        .iter()
        .filter_map(|order_client_id| {
            let wire = wire_id_for(order_client_id)?;
            (wire != *order_client_id).then(|| (order_client_id.clone(), wire))
        })
        .collect()
}

/// One row per order client id asked, in the caller's order. `confirm` is called ONLY for the order client ids that come out
/// `Querying` (so a test sees exactly which re-queries were launched).
pub fn resolve_rows(
    order_client_ids: &[String],
    tracked: impl Fn(&str) -> bool,
    venue_resolves_by_order_client_id: bool,
    mut confirm: impl FnMut(&str),
) -> Vec<OrderResolution> {
    order_client_ids
        .iter()
        .map(|order_client_id| {
            let state = resolution_of(tracked(order_client_id), venue_resolves_by_order_client_id);
            if state == ResolveState::Querying {
                confirm(order_client_id);
            }
            OrderResolution { order_client_id: order_client_id.clone(), state }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_tracked_order_is_never_re_queried_and_a_resolverless_venue_says_so() {
        assert_eq!(resolution_of(true, true), ResolveState::Tracked);
        assert_eq!(resolution_of(true, false), ResolveState::Tracked);
        assert_eq!(resolution_of(false, true), ResolveState::Querying);
        assert_eq!(resolution_of(false, false), ResolveState::NoResolver);
    }

    #[test]
    fn only_the_querying_order_client_ids_are_confirmed_and_the_rows_keep_the_callers_order() {
        let order_client_ids = strings(&["a", "b", "c"]);
        let mut confirmed = Vec::new();
        let rows =
            resolve_rows(&order_client_ids, |c| c == "b", true, |c| confirmed.push(c.to_string()));
        assert_eq!(confirmed, strings(&["a", "c"]));
        let got: Vec<_> = rows.iter().map(|r| (r.order_client_id.as_str(), r.state)).collect();
        assert_eq!(
            got,
            vec![
                ("a", ResolveState::Querying),
                ("b", ResolveState::Tracked),
                ("c", ResolveState::Querying)
            ]
        );
        let mut none = Vec::new();
        let rows = resolve_rows(&order_client_ids, |_| false, false, |c| none.push(c.to_string()));
        assert!(none.is_empty());
        assert!(rows.iter().all(|r| r.state == ResolveState::NoResolver));
    }

    #[test]
    fn wire_pairs_leave_out_the_order_client_ids_the_venue_echoes_verbatim() {
        let order_client_ids = strings(&["hl-1", "plain", "hl-2"]);
        let pairs = wire_pairs(&order_client_ids, |c| match c {
            "hl-1" | "hl-2" => Some(format!("0x{c}")),
            "plain" => Some("plain".to_string()),
            _ => None,
        });
        assert_eq!(
            pairs,
            vec![
                ("hl-1".to_string(), "0xhl-1".to_string()),
                ("hl-2".to_string(), "0xhl-2".to_string())
            ]
        );
    }
}
