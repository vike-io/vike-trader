//! `OrderVenueIdRegistry` under each re-bind rule, and `FifoMap`'s bound and its map/queue invariant.

use std::collections::HashSet;

use proptest::prelude::*;

use super::*;

/// The registry keeps both directions in step: an order client id re-bound under a new venue id leaves no stale reverse row
/// (ig's `DealIds` rule, now [`OrderVenueIdRegistry::new`]'s).
#[test]
fn a_rebind_drops_the_stale_reverse_and_forget_clears_both_directions() {
    let mut ids = OrderVenueIdRegistry::new();
    ids.bind("c1", "D1".to_string());
    assert_eq!(ids.venue_order_id_of("c1").map(String::as_str), Some("D1"));
    assert_eq!(ids.order_client_id_of("D1"), Some("c1"));
    ids.bind("c1", "D2".to_string());
    assert_eq!(ids.order_client_id_of("D1"), None, "the superseded id no longer names the order");
    assert_eq!(ids.order_client_id_of("D2"), Some("c1"));
    assert_eq!(ids.forget("c1").as_deref(), Some("D2"));
    assert_eq!((ids.venue_order_id_of("c1"), ids.order_client_id_of("D2")), (None, None));
}

/// IBKR's and polymarket's rule: both directions written with a plain insert, so the old id still names the order.
#[test]
fn keeping_the_stale_reverse_leaves_the_old_id_naming_the_order() {
    let mut ids = OrderVenueIdRegistry::keeping_stale_reverse();
    ids.bind("c1", 101_i32);
    ids.bind("c1", 102);
    assert_eq!(ids.venue_order_id_of("c1"), Some(&102));
    assert_eq!(ids.order_client_id_of(&101), Some("c1"), "the old id is kept");
    assert_eq!(ids.order_client_id_of(&102), Some("c1"));
}

#[test]
fn forgetting_by_the_venue_id_clears_both_directions() {
    let mut ids = OrderVenueIdRegistry::new();
    ids.bind("c1", 7_i64);
    ids.bind("c2", 8);
    assert_eq!(ids.forget_venue_order_id(&7).as_deref(), Some("c1"));
    assert_eq!((ids.venue_order_id_of("c1"), ids.order_client_id_of(&7)), (None, None));
    assert_eq!(ids.order_client_ids().collect::<Vec<_>>(), ["c2"]);
    assert_eq!(ids.forget_venue_order_id(&7), None, "forgotten once");
}

#[test]
fn a_fifo_map_holds_its_cap_and_evicts_the_oldest() {
    let mut t = FifoMap::<u32, 3>::default();
    for i in 0..3 {
        t.insert(format!("k{i}"), i);
    }
    assert_eq!(t.insert("k1".into(), 10), None, "a re-insert evicts nothing");
    assert_eq!(t.as_map().len(), 3, "a re-insert takes no second slot");
    assert_eq!(
        t.insert("k3".into(), 3),
        Some(("k0".to_string(), 0)),
        "the evicted row is handed back"
    );
    assert_eq!(t.as_map().len(), 3, "the table never grows past its cap");
    assert!(!t.as_map().contains_key("k0"), "the OLDEST row is the one evicted");
    assert_eq!(
        t.as_map().get("k1"),
        Some(&10),
        "a re-insert keeps its place and takes the new value"
    );
}

/// A removed row frees its slot: no live row is evicted early by a key that is no longer there.
#[test]
fn a_removed_row_frees_its_slot() {
    let mut t = FifoMap::<u32, 2>::default();
    t.insert("a".into(), 1);
    assert_eq!(t.remove("a"), Some(1));
    assert_eq!(t.remove("a"), None);
    t.insert("b".into(), 2);
    t.insert("c".into(), 3);
    assert_eq!(t.as_map().len(), 2);
    assert!(t.as_map().contains_key("b"), "no live row was evicted early");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any run of inserts and removals, evictions included, keeps the map and the queue holding the SAME keys, each
    /// once, within the cap (the property fxcm's route table was held to before it moved here).
    #[test]
    fn a_fifo_map_keeps_its_map_and_queue_in_step(
        ops in prop::collection::vec((any::<bool>(), 0u8..6), 0..48),
    ) {
        let mut t = FifoMap::<u8, 4>::default();
        for (insert, k) in ops {
            if insert {
                t.insert(k.to_string(), k);
            } else {
                let _ = t.remove(&k.to_string());
            }
            let queued: HashSet<&String> = t.inserted.iter().collect();
            prop_assert_eq!(queued.len(), t.inserted.len(), "a duplicate in the queue");
            prop_assert_eq!(queued, t.map.keys().collect::<HashSet<_>>(), "the queue and the map disagree");
            prop_assert!(t.map.len() <= 4);
        }
    }
}
