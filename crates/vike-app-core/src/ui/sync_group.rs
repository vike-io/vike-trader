//! Chart sync-group registry math (task B8) — the cross-window crosshair / visible-range
//! broadcast that links grouped chart windows. Pure `HashMap` folds over a double-buffered
//! per-frame registry; extracted from `vike-app`'s `main.rs` so the unit tests run in CI (that GUI
//! crate was only compile-checked in CI then, never tested — wgpu build weight). The shell,
//! `vike-desktop` now, re-imports [`sync_feed`] / [`sync_harvest`] / [`GroupFrame`] and wires them
//! into its frame loop.

use std::collections::HashMap;
use vike_chart::SyncIn;

/// Chart sync groups (task B8): one group's double-buffered cross-window registry
/// entry. `App::sync_next` is written while walking `App::wins` this frame (see
/// [`sync_harvest`]); at the top of next frame it becomes `App::sync_previous`, which
/// [`sync_feed`] reads to build each grouped chart window's `chart::ChartInputs::sync`.
/// `crosshair_from`/`range_from` carry the emitting window's id (`egui::Id::value()`)
/// so the emitter can filter its own broadcast back out of its own next-frame feed —
/// a window must never see its own hover/range echoed back as an injected "ghost".
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct GroupFrame {
    crosshair_timestamp: Option<i64>,
    crosshair_from: Option<u64>,
    range_timestamp: Option<(i64, i64)>,
    range_from: Option<u64>,
}

/// Build one grouped chart window's `SyncIn` from LAST frame's registry (`previous` ==
/// `App::sync_previous`): `None` when ungrouped or the group has no entry yet (nobody in
/// the group has broadcast anything). Otherwise a ghost crosshair/range is included
/// UNLESS this exact window (`wid`) is the one that emitted it — see [`GroupFrame`].
/// No symbol/interval key check: the feed is timestamp-keyed by construction, so a
/// group spanning different symbols/intervals is correct without any extra filtering
/// (task B8 step 4).
pub fn sync_feed(
    previous: &HashMap<u8, GroupFrame>,
    group: Option<u8>,
    wid: u64,
) -> Option<SyncIn> {
    let gf = previous.get(&group?)?;
    Some(SyncIn {
        crosshair_timestamp: gf.crosshair_timestamp.filter(|_| gf.crosshair_from != Some(wid)),
        range_timestamp: gf.range_timestamp.filter(|_| gf.range_from != Some(wid)),
    })
}

/// Harvest one grouped chart window's `chart::ChartActions` outputs into next frame's
/// registry (`sync_next` == `App::sync_next`) + the sticky range-leader map (`App::
/// range_leader`), called after `chart::draw`. No-op when ungrouped (`group` is
/// `None`).
///
/// Range leadership is STICKY: `interacted` (a live local drag/zoom/nav this frame)
/// claims leadership for `wid`, but once claimed, the leader keeps broadcasting its
/// `visible_timestamp` on every later frame — including frames where `interacted` is false
/// (e.g. follow-live auto-advancing the visible window) — because only the CURRENT
/// leader's `visible_timestamp` is ever written here. That's what makes follower windows
/// track a leader's live edge instead of freezing at the last manual interaction.
/// `hover_timestamp` is transient and independent of leadership: whichever window is
/// currently hovered broadcasts its crosshair, leader or not.
pub fn sync_harvest(
    sync_next: &mut HashMap<u8, GroupFrame>,
    range_leader: &mut HashMap<u8, u64>,
    group: Option<u8>,
    wid: u64,
    interacted: bool,
    visible_timestamp: Option<(i64, i64)>,
    hover_timestamp: Option<i64>,
) {
    let Some(g) = group else { return };
    if interacted {
        range_leader.insert(g, wid);
    }
    if range_leader.get(&g) == Some(&wid)
        && let Some(vts) = visible_timestamp
    {
        let gf = sync_next.entry(g).or_default();
        gf.range_timestamp = Some(vts);
        gf.range_from = Some(wid);
    }
    if let Some(hts) = hover_timestamp {
        let gf = sync_next.entry(g).or_default();
        gf.crosshair_timestamp = Some(hts);
        gf.crosshair_from = Some(wid);
    }
}

#[cfg(test)]
mod sync_group_tests {
    use super::*;

    #[test]
    fn feed_is_none_when_ungrouped() {
        let previous = HashMap::new();
        assert_eq!(sync_feed(&previous, None, 1), None);
    }

    #[test]
    fn feed_is_none_when_group_has_no_entry_yet() {
        let previous = HashMap::new();
        assert_eq!(sync_feed(&previous, Some(1), 1), None);
    }

    #[test]
    fn feed_excludes_the_emitting_window_but_not_others() {
        let mut previous = HashMap::new();
        previous.insert(
            1,
            GroupFrame {
                crosshair_timestamp: Some(100),
                crosshair_from: Some(42),
                range_timestamp: Some((10, 20)),
                range_from: Some(42),
            },
        );
        // window 42 (the emitter) must not see its own broadcast echoed back
        let own = sync_feed(&previous, Some(1), 42).unwrap();
        assert_eq!(own.crosshair_timestamp, None);
        assert_eq!(own.range_timestamp, None);
        // a different window in the same group does receive it
        let other = sync_feed(&previous, Some(1), 7).unwrap();
        assert_eq!(other.crosshair_timestamp, Some(100));
        assert_eq!(other.range_timestamp, Some((10, 20)));
    }

    #[test]
    fn feed_ignores_a_different_group() {
        let mut previous = HashMap::new();
        previous.insert(
            1,
            GroupFrame {
                crosshair_timestamp: Some(100),
                crosshair_from: Some(42),
                range_timestamp: None,
                range_from: None,
            },
        );
        assert_eq!(sync_feed(&previous, Some(2), 7), None);
    }

    #[test]
    fn harvest_is_a_no_op_when_ungrouped() {
        let mut sync_next = HashMap::new();
        let mut range_leader = HashMap::new();
        sync_harvest(&mut sync_next, &mut range_leader, None, 1, true, Some((1, 2)), Some(3));
        assert!(sync_next.is_empty());
        assert!(range_leader.is_empty());
    }

    #[test]
    fn harvest_interacted_claims_leadership_and_propagates_range() {
        let mut sync_next = HashMap::new();
        let mut range_leader = HashMap::new();
        sync_harvest(&mut sync_next, &mut range_leader, Some(1), 42, true, Some((10, 20)), None);
        assert_eq!(range_leader.get(&1), Some(&42));
        let gf = sync_next.get(&1).unwrap();
        assert_eq!(gf.range_timestamp, Some((10, 20)));
        assert_eq!(gf.range_from, Some(42));
    }

    #[test]
    fn harvest_non_leader_visible_timestamp_does_not_propagate() {
        let mut sync_next = HashMap::new();
        let mut range_leader = HashMap::new();
        range_leader.insert(1, 42); // window 42 already holds leadership
        // window 7 (not the leader) reports a visible_timestamp this frame — must not overwrite
        sync_harvest(&mut sync_next, &mut range_leader, Some(1), 7, false, Some((99, 100)), None);
        assert_eq!(range_leader.get(&1), Some(&42), "leadership must stay with 42");
        assert!(!sync_next.contains_key(&1), "a non-leader's visible_ts must not propagate");
    }

    #[test]
    fn harvest_leader_keeps_propagating_range_without_reinteracting() {
        // Sticky-leader semantics: the leader's visible_timestamp propagates even on a frame
        // where `interacted` is false (e.g. follow-live auto-advancing the range) —
        // this is what makes follower windows track the live edge.
        let mut sync_next = HashMap::new();
        let mut range_leader = HashMap::new();
        range_leader.insert(1, 42);
        sync_harvest(&mut sync_next, &mut range_leader, Some(1), 42, false, Some((5, 6)), None);
        let gf = sync_next.get(&1).unwrap();
        assert_eq!(gf.range_timestamp, Some((5, 6)));
        assert_eq!(gf.range_from, Some(42));
    }

    #[test]
    fn harvest_hover_timestamp_is_transient_and_independent_of_leadership() {
        let mut sync_next = HashMap::new();
        let mut range_leader = HashMap::new(); // no leader at all yet
        sync_harvest(&mut sync_next, &mut range_leader, Some(2), 9, false, None, Some(777));
        let gf = sync_next.get(&2).unwrap();
        assert_eq!(gf.crosshair_timestamp, Some(777));
        assert_eq!(gf.crosshair_from, Some(9));
        assert_eq!(gf.range_timestamp, None, "a hover-only harvest must not touch the range side");
    }
}
