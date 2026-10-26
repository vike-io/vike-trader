//! `WireIds` and `ResolveOrders`: the two QUERY-ONLY request frames of a lease.
//!
//! Neither places, cancels or amends anything. Both run on the account's dispatch thread (the one that
//! owns the `ExecutionClient`), in order with the commands before them, and answer through the lease that
//! asked.
//!
//! * **`WireIds{coids}`** is the boot prefetch of restored client order ids: for every coid whose wire id
//!   (`ExecutionClient::wire_id_for`) is a pure function of the coid and NOT the coid itself (hyperliquid's
//!   `cloid = keccak(coid)`), a `(coid, wire id)` pair. A coid the venue echoes verbatim is left out.
//! * **`ResolveOrders{coids}`** is the query-only resolution of Written-but-unacknowledged submits after
//!   an epoch change (B.7 of the wave-2 plan). Per coid the daemon says what it did: the order is one it
//!   TRACKS (its events flow, or already flowed), it launched the venue's re-query by client order id
//!   (`confirm`), or the venue has no such lookup and the order stays UNKNOWN until reconcile. It never
//!   sends anything a second time and never invents an event.

use vike_gatewayhub_client::proto::{OrderResolution, ResolveState};

/// What to do about one coid, from the two facts the daemon has.
pub fn resolution_of(tracked: bool, venue_resolves_by_coid: bool) -> ResolveState {
    if tracked {
        ResolveState::Tracked
    } else if venue_resolves_by_coid {
        ResolveState::Querying
    } else {
        ResolveState::NoResolver
    }
}

/// The `(coid, wire id)` pairs of `coids` whose wire id differs from the coid, in the caller's order.
pub fn wire_pairs(
    coids: &[String],
    wire_id_for: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    coids
        .iter()
        .filter_map(|coid| {
            let wire = wire_id_for(coid)?;
            (wire != *coid).then(|| (coid.clone(), wire))
        })
        .collect()
}

/// One row per coid asked, in the caller's order. `confirm` is called ONLY for the coids that come out
/// `Querying` (so a test sees exactly which re-queries were launched).
pub fn resolve_rows(
    coids: &[String],
    tracked: impl Fn(&str) -> bool,
    venue_resolves_by_coid: bool,
    mut confirm: impl FnMut(&str),
) -> Vec<OrderResolution> {
    coids
        .iter()
        .map(|coid| {
            let state = resolution_of(tracked(coid), venue_resolves_by_coid);
            if state == ResolveState::Querying {
                confirm(coid);
            }
            OrderResolution { coid: coid.clone(), state }
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
    fn only_the_querying_coids_are_confirmed_and_the_rows_keep_the_callers_order() {
        let coids = strings(&["a", "b", "c"]);
        let mut confirmed = Vec::new();
        let rows = resolve_rows(&coids, |c| c == "b", true, |c| confirmed.push(c.to_string()));
        assert_eq!(confirmed, strings(&["a", "c"]));
        let got: Vec<_> = rows.iter().map(|r| (r.coid.as_str(), r.state)).collect();
        assert_eq!(
            got,
            vec![
                ("a", ResolveState::Querying),
                ("b", ResolveState::Tracked),
                ("c", ResolveState::Querying)
            ]
        );
        let mut none = Vec::new();
        let rows = resolve_rows(&coids, |_| false, false, |c| none.push(c.to_string()));
        assert!(none.is_empty());
        assert!(rows.iter().all(|r| r.state == ResolveState::NoResolver));
    }

    #[test]
    fn wire_pairs_leave_out_the_coids_the_venue_echoes_verbatim() {
        let coids = strings(&["hl-1", "plain", "hl-2"]);
        let pairs = wire_pairs(&coids, |c| match c {
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
