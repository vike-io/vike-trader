//! "Arbitrary input never panics" harness for the PRIVATE order table the exec drain loop keeps:
//! [`RouteTable`] (venue order id -> order client id, over the shared FIFO-bounded
//! `vike_bridge_core::execution_client::order_client_id::FifoMap`), driven the way `drain_events` drives it — each
//! shim envelope decoded through the real `map_drained_event` against `RouteTable::routes`, and the route
//! dropped once the envelope closes it. The loop itself is welded to a live `FxcmSession`'s
//! `poll_event`, so this replays its body over text lines. The public decoders are covered in
//! `crates/bridges/fxcm/tests/decoder_never_panics.rs`.
//!
//! The property is TOTALITY plus the table's answers: after any run of inserts and removals each id routes to
//! what it was last given or not at all, and the table stays within [`MAX_ROUTES`]. That the map and the queue agree
//! is `FifoMap`'s own property; eviction at the cap is pinned by `execution/execution_tests.rs`; random sequences stay far below it
//! by construction.

use super::*;
use proptest::prelude::*;

/// One operation on the table.
#[derive(Debug, Clone)]
enum Op {
    Insert(String, String),
    Remove(String),
}

/// Venue order ids from a small alphabet (so inserts collide and removes hit) or free text.
fn arb_id() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(vec!["O1", "O2", "O3", "O4", ""]).prop_map(String::from),
        1 => any::<String>(),
    ]
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (arb_id(), any::<String>())
            .prop_map(|(id, order_client_id)| Op::Insert(id, order_client_id)),
        arb_id().prop_map(Op::Remove),
    ]
}

/// One drained envelope line: real-shaped fill / cancel / reject for the small id alphabet, or text.
fn arb_line() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => (
            prop::sample::select(vec!["fill", "canceled", "rejected", "other"]),
            prop::sample::select(vec!["O1", "O2", "O3", "O4", ""]),
            "[A-Z0-9]{0,6}",
            prop::sample::select(vec!["10000", "0", "-5", "1e999", "\"x\"", "null"]),
        )
            .prop_map(|(kind, oid, trade, amount)| {
                format!(
                    r#"{{"kind":"{kind}","order_id":"{oid}","trade_id":"{trade}","instrument":"EUR/USD","side":"S","amount":{amount},"rate":1.0912,"commission":0.08,"timestamp":0}}"#
                )
            }),
        1 => any::<String>(),
        1 => prop::collection::vec(any::<u8>(), 0..256)
            .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A run of inserts and removals routes each id to what it was last given, or not at all once removed.
    #[test]
    fn route_table_stays_consistent_under_any_op_sequence(
        ops in prop::collection::vec(arb_op(), 0..24),
    ) {
        let mut table = RouteTable::default();
        for op in ops {
            match op {
                Op::Insert(id, order_client_id) => {
                    table.insert(id.clone(), order_client_id.clone());
                    prop_assert_eq!(table.routes().get(&id), Some(&order_client_id));
                }
                Op::Remove(id) => {
                    table.remove(&id);
                    prop_assert!(!table.routes().contains_key(&id));
                }
            }
            prop_assert!(table.routes().len() <= MAX_ROUTES);
        }
    }

    /// `drain_events`' body over text lines: parse, route, decode, drop a closed route. Never a
    /// panic, never more than the dual-publish pair per line, and a CLOSED order leaves the table.
    #[test]
    fn the_drain_loop_survives_a_line_sequence(
        lines in prop::collection::vec(arb_line(), 1..12),
        routed in prop::collection::vec(prop::sample::select(vec!["O1", "O2", "O3"]), 0..4),
    ) {
        let mut table = RouteTable::default();
        for id in &routed {
            table.insert((*id).to_string(), format!("order-client-id-{id}"));
        }
        for line in &lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let events = map_drained_event(&v, table.routes());
            prop_assert!(events.len() <= 2, "event flood: {} events", events.len());
            let _ = ends_cancelability(&events);
            let _ = order_client_id_of(&events);
            if closes_routing(&events) {
                let oid = v.get("order_id").and_then(|x| x.as_str()).unwrap_or_default();
                table.remove(oid);
                prop_assert!(!table.routes().contains_key(oid), "a closed route survived");
            }
        }
    }

    /// The request-side symbol mapping is total over arbitrary text (non-ASCII included — the
    /// `[..3]` split is guarded by an all-ASCII check).
    #[test]
    fn instrument_mapping_survives_arbitrary_text(symbol in any::<String>()) {
        let _ = to_fxcm_instrument(&symbol);
    }
}
