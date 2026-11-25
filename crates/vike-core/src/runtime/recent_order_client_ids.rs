//! **The late-retry memory (decision 0270, D7): order client ids this node saw before its last restart.**
//!
//! Built ONCE at boot (`crate::replay`'s restore hands it in as `CoreConfig::recent_order_client_ids`), never written while the
//! process runs (the engine registries, which never evict, cover the ids this process sees), consulted by every door
//! that takes a caller's explicit id, and carried WHOLE into every `Snapshot`. The carried form is the journal's:
//! `(order client id, created_ms)` pairs.
//!
//! **A `Snapshot` also carries the ids of the ENDED orders its engines leave out** (`Engine::journal_snapshot`
//! keeps a registry bounded by what is live; `CoreThread::write_snap`'s `note_left_out`): the
//! engine registry is no longer where the next boot finds them, so they ride here, those created within
//! [`RECENT_ORDER_CLIENT_ID_WINDOW_MS`] of the `Snapshot`'s clock (the span the boot keeps of the memory anyway), under the cap.
//!
//! ⚠ **The carried pairs are pruned at boot only, never in the running process or at a `Snapshot`.** A replay core refolding
//! from a `Snapshot` this process wrote must refuse exactly what this process refused; a `Snapshot` trimmed by its own clock
//! would let the replay accept a retry the live core refused (an id that aged past the window between the boot and the
//! `Snapshot`). The [`RECENT_ORDER_CLIENT_ID_CAP`] bounds it either way. The one such residual left: a retry, in the tail after a
//! `Snapshot`, of an ended order's id older than the window at that `Snapshot` (the live core still finds it in its registry,
//! a replay core from the `Snapshot` does not), the very retry the next boot would accept too.

use std::collections::HashSet;

/// An id is remembered for this long after its order's `created_ms`: the same 24 h span IBKR Client Portal's `cOID`
/// must be unique for. A constant, not a setting.
pub(crate) const RECENT_ORDER_CLIENT_ID_WINDOW_MS: i64 = vike_model::MS_PER_DAY;

/// At most this many ids are carried, the newest kept. A constant, not a setting.
pub(crate) const RECENT_ORDER_CLIENT_ID_CAP: usize = 65_536;

/// Prune the carried pairs in place: drop ids created before `now_ms - RECENT_ORDER_CLIENT_ID_WINDOW_MS`, keep ONE pair per id
/// (its newest `created_ms`), then keep the newest [`RECENT_ORDER_CLIENT_ID_CAP`]. The result is sorted by `(created_ms, id)`,
/// so the same input always carries the same bytes. The boot restore's one call
/// (`crates/vike-core/src/replay/boot.rs`'s `boot_memory`) runs it at the restore's own clock read.
pub(crate) fn prune_carried(carried: &mut Vec<(String, i64)>, now_ms: i64) {
    let floor = now_ms.saturating_sub(RECENT_ORDER_CLIENT_ID_WINDOW_MS);
    carried.retain(|(_, at)| *at >= floor);
    sort_and_cap(carried);
}

/// One pair per id (its newest), the newest [`RECENT_ORDER_CLIENT_ID_CAP`], sorted by `(created_ms, id)`.
fn sort_and_cap(carried: &mut Vec<(String, i64)>) {
    // Newest first per id, so `dedup_by` keeps each id's newest pair.
    carried.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    carried.dedup_by(|later, kept| later.0 == kept.0);
    carried.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    if carried.len() > RECENT_ORDER_CLIENT_ID_CAP {
        carried.drain(..carried.len() - RECENT_ORDER_CLIENT_ID_CAP);
    }
}

/// The memory a core holds: the carried pairs (for the next `Snapshot`) and a set for the doors' O(1) consult.
#[derive(Debug, Default)]
pub(crate) struct RecentOrderClientIds {
    carried: Vec<(String, i64)>,
    ids: HashSet<String>,
}

impl RecentOrderClientIds {
    /// The memory of `carried` as boot handed it (the restore prunes it to the window with its own clock read; the
    /// core reads none here, so a replay core stays clock-free). Sorted and capped, never trimmed by time.
    pub(crate) fn new(mut carried: Vec<(String, i64)>) -> RecentOrderClientIds {
        sort_and_cap(&mut carried);
        let ids = carried.iter().map(|(id, _)| id.clone()).collect();
        RecentOrderClientIds { carried, ids }
    }

    /// Whether `id` was used by an order before this node's last restart, within the window.
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// The pairs to carry into a `Snapshot`: all of them, exactly as boot handed them (see the module doc's ⚠). No
    /// allocation; empty, so no key on the wire, on a core given none.
    pub(crate) fn carried(&self) -> &[(String, i64)] {
        &self.carried
    }

    /// [`Self::carried`] with `left_out`, the ids of the ended orders the `Snapshot`'s engines left out (`write_snap`):
    /// one pair per id, the newest [`RECENT_ORDER_CLIENT_ID_CAP`], sorted. The carried pairs are never trimmed by time.
    pub(crate) fn carried_with(&self, mut left_out: Vec<(String, i64)>) -> Vec<(String, i64)> {
        left_out.extend_from_slice(&self.carried);
        sort_and_cap(&mut left_out);
        left_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_the_window_one_pair_per_id_and_the_newest_cap() {
        let now = 10 * RECENT_ORDER_CLIENT_ID_WINDOW_MS;
        let mut v = vec![
            ("old".to_string(), now - RECENT_ORDER_CLIENT_ID_WINDOW_MS - 1),
            ("b".to_string(), now - 5),
            ("a".to_string(), now - 9),
            ("b".to_string(), now - 7),
            ("edge".to_string(), now - RECENT_ORDER_CLIENT_ID_WINDOW_MS),
        ];
        prune_carried(&mut v, now);
        let want =
            [("edge", now - RECENT_ORDER_CLIENT_ID_WINDOW_MS), ("a", now - 9), ("b", now - 5)];
        assert_eq!(v.iter().map(|(i, t)| (i.as_str(), *t)).collect::<Vec<_>>(), want);

        let mut many: Vec<(String, i64)> = (0..RECENT_ORDER_CLIENT_ID_CAP as i64 + 3)
            .map(|i| (format!("c{i}"), now - i))
            .collect();
        prune_carried(&mut many, now);
        assert_eq!(many.len(), RECENT_ORDER_CLIENT_ID_CAP);
        assert!(!many.iter().any(|(id, _)| id == "c65538"), "the oldest goes first");
        assert!(many.iter().any(|(id, _)| id == "c0"));

        let memory = RecentOrderClientIds::new(v);
        assert!(memory.contains("a") && !memory.contains("old"));
        let carried: Vec<&str> = memory.carried().iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(carried, ["edge", "a", "b"], "carried whole, oldest first, whatever the clock");
    }
}
