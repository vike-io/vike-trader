//! `ChartState` cache tests: incremental `sync`, the visible volume max, the volume profile and CVD.

use super::testutil::{bar, raw_bar, wire_bar_at};
use super::*;

/// Oracle: a fresh full clear+rebuild of `(closed, forming)` via `render_bar` —
/// exactly what today's `sync_from_core` loop (and `sync`'s own full-rebuild
/// branch) produces. `sync`'s incremental output must equal this byte-for-byte
/// after every call, no matter which internal branch it took.
fn full_rebuild(
    closed: &[vike_marketdata::Bar],
    forming: Option<&vike_marketdata::Bar>,
) -> Vec<Bar> {
    let mut bars: Vec<Bar> = closed.iter().enumerate().map(|(i, b)| render_bar(i, b)).collect();
    if let Some(f) = forming {
        let idx = bars.len();
        bars.push(render_bar(idx, f));
    }
    bars
}

/// `sync` then assert `cs.bars` equals a fresh full rebuild of the same
/// `(closed, forming)` — the equivalence gate for every step of a tick sequence.
fn sync_and_check_equivalence(
    cs: &mut ChartState,
    closed: &[vike_marketdata::Bar],
    forming: Option<&vike_marketdata::Bar>,
    label: &str,
) {
    cs.sync(&std::sync::Arc::new(closed.to_vec()), forming);
    assert_eq!(cs.bars, full_rebuild(closed, forming), "{label}: bars diverged from full rebuild");
}

/// `sync` must be byte-identical to a full clear+rebuild at every step of a
/// realistic tick sequence: seed, five forming-only ticks, a single bar close, a
/// forming tick after that close, three bars closing at once (GUI polled less
/// often than bars closed), then a reload onto a shorter, differently-shaped
/// series (symbol/timeframe swap). Exercises all three `sync` branches
/// (unchanged-prefix, append, full-rebuild) against the same oracle.
#[test]
fn sync_matches_full_rebuild_over_tick_sequence() {
    let mut cs = ChartState::default();
    let mut closed: Vec<vike_marketdata::Bar> = (0..100).map(|i| raw_bar(i, 0.3)).collect();
    let mut forming = raw_bar(100, 0.0);

    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "seed");

    for k in 0..5 {
        forming.close += (k + 1) as f64 * 0.02;
        forming.high = forming.high.max(forming.close);
        sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "forming-tick");
    }

    closed.push(forming); // bar 100 closes
    forming = raw_bar(101, -0.1);
    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "close 1");

    forming.close -= 0.03;
    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "forming-tick after close 1");

    closed.push(forming); // bar 101 closes
    closed.push(raw_bar(102, 0.15)); // + two more that closed while unpolled
    closed.push(raw_bar(103, -0.25));
    forming = raw_bar(104, 0.0);
    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "close 3");

    let reload_closed: Vec<vike_marketdata::Bar> = (0..40).map(|i| raw_bar(i, -0.5)).collect();
    let reload_forming = raw_bar(40, 0.2);
    sync_and_check_equivalence(&mut cs, &reload_closed, Some(&reload_forming), "reload");
}

/// `sync` does O(delta) rendering work, not O(history): the work counter must
/// read 0 on an unchanged-prefix forming tick, the exact delta on an append (1
/// bar, then 3 at once), and the full length again on a structural reload — never
/// more than the bars that actually changed.
#[test]
fn sync_does_delta_work_only() {
    let mut cs = ChartState::default();
    let mut closed: Vec<vike_marketdata::Bar> = (0..100).map(|i| raw_bar(i, 0.3)).collect();
    let mut forming = raw_bar(100, 0.0);

    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 100, "seed renders every closed bar");
    assert!(cs.last_sync_work.forming_rerendered);

    forming.close += 0.05;
    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 0, "forming tick touches no closed bars");

    closed.push(forming); // bar 100 closes
    forming = raw_bar(101, -0.1);
    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 1, "one bar closed");

    closed.push(forming); // bar 101 closes
    closed.push(raw_bar(102, 0.15));
    closed.push(raw_bar(103, -0.25));
    forming = raw_bar(104, 0.0);
    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 3, "three bars closed at once");

    let reload_closed: Vec<vike_marketdata::Bar> = (0..40).map(|i| raw_bar(i, -0.5)).collect();
    cs.sync(&std::sync::Arc::new(reload_closed.clone()), None);
    assert_eq!(cs.last_sync_work.closed_rerendered, 40, "reload is a full rebuild");
    assert!(!cs.last_sync_work.forming_rerendered);
}

/// `set_tz` must invalidate `hour_marks` on the very next `sync`, even though the closed
/// bars themselves are byte-identical (so `sync`'s own bars-content dedup takes the
/// "unchanged prefix" fast path) — `refresh_caches`'s `cache_key` carries the tz identity
/// specifically so this doesn't go stale.
#[test]
fn set_tz_invalidates_mark_cache() {
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z").unwrap().timestamp_millis();
    let closed: Vec<vike_marketdata::Bar> =
        (0..4).map(|i| wire_bar_at(base + i as i64 * 900_000)).collect();
    let mut cs = ChartState::default();
    cs.set_tz(DisplayTz::Utc);
    cs.sync(&std::sync::Arc::new(closed.clone()), None);
    assert_eq!(cs.hour_marks, vec![2.0]);
    cs.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
    cs.sync(&std::sync::Arc::new(closed.clone()), None); // same bars, new tz → cache must refresh
    assert_eq!(cs.hour_marks, vec![0.0]);
}

/// [`ChartState::visible_vol_max`] must cache by `(lo, hi, cache_key)`: a repeat call
/// with the same window is an O(1) cache hit (no work-counter bump); a different
/// window, or a bar CLOSE (which bumps `cache_key`), forces a genuine recompute. Every
/// returned value must also equal a plain fold over the CLOSED window
/// `bars[lo..hi.min(closed_len)]` — the oracle this cache must never diverge from.
#[test]
fn visible_vol_max_caches_by_key() {
    // 100 closed bars, v = 1.0..=100.0 (bar i has v = i + 1).
    let mut cs = ChartState {
        bars: (0..100)
            .map(|i| {
                let mut b = bar(i, 10.0, 20.0);
                b.v = (i + 1) as f64;
                b
            })
            .collect(),
        closed_len: 100,
        ..Default::default()
    };
    cs.refresh_caches(); // seeds cache_key

    // oracle: a plain fold over the CLOSED window, matching visible_vol_max's contract.
    let oracle = |cs: &ChartState, lo: usize, hi: usize| -> f64 {
        let hi = hi.min(cs.closed_len);
        let lo = lo.min(hi);
        cs.bars[lo..hi].iter().map(|b| b.v).fold(0.0_f64, f64::max)
    };

    assert_eq!(cs.visible_vol_max(0, 50), 50.0, "max of v=1..=50 is 50.0");
    assert_eq!(cs.visible_vol_max(0, 50), oracle(&cs, 0, 50));
    assert_eq!(cs.vol_recompute_count.get(), 1, "first call is a genuine miss");

    // same (lo, hi), same cache_key -> cache hit, no recompute.
    assert_eq!(cs.visible_vol_max(0, 50), 50.0);
    assert_eq!(cs.vol_recompute_count.get(), 1, "repeat call must be a cache hit");

    // different window -> recompute (count bumps).
    assert_eq!(cs.visible_vol_max(10, 60), oracle(&cs, 10, 60));
    assert_eq!(cs.vol_recompute_count.get(), 2, "a different (lo, hi) must miss");

    // repeat the NEW window -> cache hit again.
    assert_eq!(cs.visible_vol_max(10, 60), oracle(&cs, 10, 60));
    assert_eq!(cs.vol_recompute_count.get(), 2, "repeat of the new window must hit");

    // close a bar (bumps cache_key via refresh_caches) -> same (lo, hi) now misses.
    let mut closing = bar(100, 10.0, 20.0);
    closing.v = 999.0; // outside [10, 60), so the oracle value is unchanged...
    cs.bars.push(closing);
    cs.closed_len = 101;
    cs.refresh_caches(); // bumps cache_key (closed_len 100 -> 101)
    assert_eq!(
        cs.visible_vol_max(10, 60),
        oracle(&cs, 10, 60),
        "value is unchanged (bar 100 is outside [10,60)), but the key changed"
    );
    assert_eq!(
        cs.vol_recompute_count.get(),
        3,
        "a bar close must force a recompute even for the same window"
    );

    // empty window -> 0.0 (the fold's zero identity), and does not panic.
    assert_eq!(cs.visible_vol_max(60, 10), 0.0, "lo > hi collapses to an empty window");
    assert_eq!(cs.visible_vol_max(200, 300), 0.0, "window entirely past closed_len");
}

/// [`ChartState::visible_vol_max`] only ever folds the CLOSED prefix — a forming bar
/// (`bars[closed_len..]`) sitting inside `[lo, hi)` must NOT be picked up, even though
/// today's inline `visible_slice(bars, ..)` fold (which callers replace with this cache
/// PLUS their own `.max(forming.v)`) would see it as just the last element. This is the
/// forming-visible/forming-hidden equivalence the two call sites depend on.
#[test]
fn visible_vol_max_excludes_the_forming_bar() {
    let mut cs = ChartState {
        bars: (0..10)
            .map(|i| {
                let mut b = bar(i, 10.0, 20.0);
                b.v = 5.0; // closed bars: uniform v=5.0
                b
            })
            .collect(),
        closed_len: 9, // bar index 9 is forming
        ..Default::default()
    };
    cs.bars[9].v = 1_000.0; // forming bar: wildly larger volume
    cs.refresh_caches();

    // forming bar (index 9) IS inside [0, 10) -> must still be excluded from the CLOSED-only max.
    assert_eq!(
        cs.visible_vol_max(0, 10),
        5.0,
        "forming bar's huge v must not leak into the closed-only max"
    );

    // sanity: the SAME window over the full `bars` slice (closed + forming), i.e. what
    // today's inline fold computes, DOES pick up the forming bar — proving the two are
    // deliberately different, and that callers must .max() the forming value in themselves.
    let full_fold = cs.bars[0..10].iter().map(|b| b.v).fold(0.0_f64, f64::max);
    assert_eq!(full_fold, 1_000.0);
}

/// `visible_vol_max` returns [`ChartState::visible_vol_max_shared`]'s value exactly —
/// the `&mut self` entry point is a thin forwarder, not a second cache/implementation.
#[test]
fn visible_vol_max_matches_shared_twin() {
    let mut cs = ChartState {
        bars: (0..20)
            .map(|i| {
                let mut b = bar(i, 10.0, 20.0);
                b.v = (i * 3) as f64;
                b
            })
            .collect(),
        closed_len: 20,
        ..Default::default()
    };
    let oracle = cs.bars[2..15].iter().map(|b| b.v).fold(0.0_f64, f64::max);
    assert_eq!(cs.visible_vol_max_shared(2, 15), oracle);
    assert_eq!(
        cs.visible_vol_max(2, 15),
        oracle,
        "&mut self entry point must agree with the &self twin"
    );
}

// === SP2 T4: visible-range volume-profile overlay cache ================================

fn fp(idx: u64, cells: &[(f64, f64, f64)]) -> FootprintBar {
    FootprintBar {
        bar_index: idx,
        cells: cells
            .iter()
            .map(|&(p, b, s)| vike_orderflow::PriceBin { price: p, buy_vol: b, sell_vol: s })
            .collect(),
    }
}

/// [`ChartState::visible_profile`] must cache by [`ProfileCacheKey`]: a repeat call with
/// the same `(i0, i1, footprints.len(), tick_size)` is an O(1) cache hit (no recompute-
/// counter bump); a different range, a different footprint length, or a different tick
/// size each force a genuine recompute. Every returned value must also equal a direct
/// [`VolumeProfile::from_footprints`] call — the oracle this cache must never diverge from.
#[test]
fn visible_profile_caches_by_key() {
    let cs = ChartState::default();
    let fps = vec![
        fp(0, &[(100.0, 5.0, 2.0)]),
        fp(1, &[(100.0, 1.0, 0.0), (101.0, 0.0, 4.0)]),
        fp(2, &[(101.0, 3.0, 0.0)]),
    ];
    let oracle = |ts: f64, i0: usize, i1: usize| VolumeProfile::from_footprints(&fps, ts, i0, i1);

    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 2), oracle(1.0, 0, 2));
    assert_eq!(cs.profile_recompute_count.get(), 1, "first call is a genuine miss");

    // same (i0, i1, len, tick_size) -> cache hit, no recompute.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 2), oracle(1.0, 0, 2));
    assert_eq!(cs.profile_recompute_count.get(), 1, "repeat call must be a cache hit");

    // different range -> miss.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 1, 2), oracle(1.0, 1, 2));
    assert_eq!(cs.profile_recompute_count.get(), 2, "a different (i0, i1) must miss");

    // repeat the new range -> hit.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 1, 2), oracle(1.0, 1, 2));
    assert_eq!(cs.profile_recompute_count.get(), 2, "repeat of the new range must hit");

    // same range, different tick_size -> miss (bucket width changed, len unchanged).
    assert_eq!(*cs.visible_profile_shared(&fps, 2.0, 1, 2), oracle(2.0, 1, 2));
    assert_eq!(cs.profile_recompute_count.get(), 3, "a different tick_size must miss");

    // shorter footprint slice (len changes, same range/tick_size) -> miss. Oracle is over
    // the SAME shortened slice — `from_footprints` clamps `hi` to the slice's own length,
    // so comparing against the full-`fps` oracle would silently check a different
    // computation (a different `hi` clamp) rather than the cache-vs-direct equivalence.
    let short = &fps[..2];
    assert_eq!(
        *cs.visible_profile_shared(short, 2.0, 1, 2),
        VolumeProfile::from_footprints(short, 2.0, 1, 2)
    );
    assert_eq!(cs.profile_recompute_count.get(), 4, "a different footprint length must miss");
}

/// `visible_profile` returns [`ChartState::visible_profile_shared`]'s value exactly — the
/// `&mut self` entry point is a thin forwarder, not a second cache/implementation.
#[test]
fn visible_profile_matches_shared_twin() {
    let mut cs = ChartState::default();
    let fps = vec![fp(0, &[(50.0, 2.0, 1.0)]), fp(1, &[(51.0, 0.0, 3.0)])];
    let oracle = VolumeProfile::from_footprints(&fps, 1.0, 0, 1);
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 1), oracle);
    assert_eq!(
        *cs.visible_profile(&fps, 1.0, 0, 1),
        oracle,
        "&mut self entry point must agree with the &self twin"
    );
}

/// Empty/out-of-range windows must not panic and must match
/// [`VolumeProfile::from_footprints`]'s own documented empty behavior (poc 0.0, va (0,0),
/// no bins) — the cache is a pure memoization layer, never a second source of truth for
/// edge-case handling.
#[test]
fn visible_profile_empty_range_is_the_from_footprints_default() {
    let cs = ChartState::default();
    let fps = vec![fp(0, &[(100.0, 5.0, 2.0)])];
    let empty = cs.visible_profile_shared(&fps, 1.0, 5, 9);
    assert!(empty.bins.is_empty());
    assert_eq!(empty.poc, 0.0);
    assert_eq!(empty.value_area, (0.0, 0.0));
    // lo > hi also collapses to the same empty result, matching from_footprints directly.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 2, 1), *empty);
}

// === SP3 Task B #2 (SP2 final-review finding B) + SP3 TB-fix (post-Task-B review finding,
// MEDIUM: generation-hardened against ABA staleness) CVD pane recompute cache ============

/// [`ChartState::cvd_shared`] must cache by [`CvdCacheKey`] (`(generation, len)`): a repeat
/// call at the SAME generation+len is an O(1) cache hit — no recompute-counter bump, and the
/// exact same `Rc` handed back (proving it wasn't just a coincidental content match from an
/// independent recompute) — while a generation bump is ALWAYS a genuine miss, even over the
/// byte-identical slice at the SAME address. That last property is the ABA-hardening fix
/// itself under direct test: the old `(ptr, len)` key would have wrongly served the stale
/// cached `Rc` here (same address, same length); keying on the caller-supplied generation
/// instead means a bumped generation is trusted as "content changed" without re-deriving that
/// from the slice's own (recyclable) address. Every returned value must equal a direct
/// `orderflow::cvd_from_footprints` call — the oracle this cache must never diverge from.
#[test]
fn cvd_shared_caches_by_generation_and_len_not_pointer_identity() {
    let cs = ChartState::default();
    let fps = vec![
        fp(0, &[(100.0, 5.0, 2.0)]),                    // delta +3
        fp(1, &[(101.0, 1.0, 4.0)]),                    // delta -3
        fp(2, &[(100.0, 2.0, 0.0), (101.0, 0.0, 1.0)]), // delta +1
    ];
    let oracle = crate::orderflow::cvd_from_footprints(&fps);

    let first = cs.cvd_shared(&fps, 1);
    assert_eq!(*first, oracle);
    assert_eq!(cs.cvd_recompute_count.get(), 1, "first call is a genuine miss");

    // same slice (same address AND length), same generation -> cache hit: same Rc, no
    // recompute.
    let second = cs.cvd_shared(&fps, 1);
    assert!(
        Rc::ptr_eq(&first, &second),
        "a repeat call at the SAME generation must hand back the SAME Rc"
    );
    assert_eq!(cs.cvd_recompute_count.get(), 1, "repeat call must be a cache hit");

    // the IDENTICAL slice (same address, same length, same content) but a bumped generation
    // -> must MISS. This is the ABA scenario the fix targets: a toggle-off/on cycle can hand
    // `cvd_shared` a slice whose address+length coincidentally match the stale cached key even
    // though the content is logically new; the generation is the caller's authoritative signal
    // that it changed, and must win over the slice's own identity.
    let third = cs.cvd_shared(&fps, 2);
    assert_eq!(*third, oracle);
    assert_eq!(
        cs.cvd_recompute_count.get(),
        2,
        "a generation bump must miss even over the identical slice"
    );

    // a shorter slice of the SAME underlying allocation, SAME generation as `third` -> still
    // a miss (len is a belt-and-suspenders second key component).
    let shorter = &fps[..2];
    let fourth = cs.cvd_shared(shorter, 2);
    assert_eq!(*fourth, crate::orderflow::cvd_from_footprints(shorter));
    assert_eq!(cs.cvd_recompute_count.get(), 3, "a different length must miss");

    // repeat the full slice at generation 1 again -> miss (the slot now holds generation 2's
    // key from the calls above).
    let fifth = cs.cvd_shared(&fps, 1);
    assert_eq!(*fifth, oracle);
    assert_eq!(
        cs.cvd_recompute_count.get(),
        4,
        "the slot was evicted by the later-generation calls"
    );
}

/// Empty footprints must not panic and must match `cvd_from_footprints(&[])`'s own
/// documented empty behavior (an empty `Vec`) — the cache is a pure memoization layer, never
/// a second source of truth for edge-case handling.
#[test]
fn cvd_shared_empty_footprints_is_empty() {
    let cs = ChartState::default();
    assert!(cs.cvd_shared(&[], 0).is_empty());
}
