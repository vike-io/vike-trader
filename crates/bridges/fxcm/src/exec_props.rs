//! "Arbitrary input never panics" harness for the PRIVATE order table the exec drain loop keeps:
//! [`RouteTable`] (venue order id -> client order id, a FIFO-bounded pair of a map and an insertion
//! queue that must always hold the SAME set), driven the way `drain_events` drives it — each shim
//! envelope decoded through the real `map_drained_event` against `RouteTable::routes`, and the route
//! dropped once the envelope closes it. The loop itself is welded to a live `FxcmSession`'s
//! `poll_event`, so this replays its body over text lines. The public decoders are covered in
//! `crates/bridges/fxcm/tests/decoder_never_panics.rs`.
//!
//! The property is TOTALITY plus the table's own invariant: after any run of inserts and removals
//! the map and the queue agree, and the table stays within [`MAX_ROUTES`]. Eviction at the cap is
//! pinned by `exec_tests.rs`; random sequences stay far below it by construction.

use super::*;
use proptest::prelude::*;
use std::collections::HashSet;

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
        (arb_id(), any::<String>()).prop_map(|(id, coid)| Op::Insert(id, coid)),
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
                    r#"{{"kind":"{kind}","order_id":"{oid}","trade_id":"{trade}","instrument":"EUR/USD","side":"S","amount":{amount},"rate":1.0912,"commission":-0.08,"ts":0}}"#
                )
            }),
        1 => any::<String>(),
        1 => prop::collection::vec(any::<u8>(), 0..256)
            .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A run of inserts and removals keeps the map and the queue in step.
    #[test]
    fn route_table_stays_consistent_under_any_op_sequence(
        ops in prop::collection::vec(arb_op(), 0..24),
    ) {
        let mut table = RouteTable::default();
        for op in ops {
            match op {
                Op::Insert(id, coid) => table.insert(id, coid),
                Op::Remove(id) => table.remove(&id),
            }
            let queued: HashSet<&String> = table.inserted.iter().collect();
            let mapped: HashSet<&String> = table.map.keys().collect();
            prop_assert_eq!(queued.len(), table.inserted.len(), "a duplicate in the queue");
            prop_assert_eq!(queued, mapped, "the queue and the map disagree");
            prop_assert!(table.map.len() <= MAX_ROUTES);
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
            table.insert((*id).to_string(), format!("coid-{id}"));
        }
        for line in &lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let evs = map_drained_event(&v, table.routes());
            prop_assert!(evs.len() <= 2, "event flood: {} events", evs.len());
            let _ = ends_cancelability(&evs);
            let _ = coid_of(&evs);
            if closes_routing(&evs) {
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
