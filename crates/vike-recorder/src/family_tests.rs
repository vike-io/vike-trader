use super::liveness_tests::live;
use super::*;

const FAM: &str = "book/polymarket/btc-updown-5m";
const OUT: &str = "trade/binance/BTCUSDT.P";
/// Half a window, so every fixture below has an inside and an edge.
const HALF: i64 = FAMILY_WINDOW_MS / 2;
/// A baseline comfortably over [`MIN_BASELINE_ITEMS`], so the fixtures are judged rather than
/// refused for want of resolution.
const BASELINE: u64 = 20_000;

fn fam(keys: &[&str]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> =
        keys.iter().map(|k| (k.to_string(), FAM.to_string())).collect();
    // The licence, which every verdict needs: a series OUTSIDE the judged family.
    v.push((OUT.to_string(), OUT.to_string()));
    v
}

/// A running arrival record — `rows` monotonic per key, never reset and never removed, exactly
/// as `crates/vike-data/src/live_rec.rs`'s `ingest` maintains the real one.
#[derive(Default)]
struct Tape {
    rows: HashMap<String, u64>,
    now: i64,
}

impl Tape {
    fn add(&mut self, key: &str, items: u64) {
        *self.rows.entry(key.to_string()).or_insert(0) += items;
    }
    fn live(&self) -> HashMap<String, Liveness> {
        live(&self.rows.iter().map(|(k, r)| (k.as_str(), *r, self.now)).collect::<Vec<_>>())
    }
}

/// Prime `watch`'s ring to FULL at [`BASELINE`], one window per tick, and return the tape.
fn primed(watch: &mut SilenceWatch, keys: &[&str]) -> Tape {
    let f = fam(keys);
    let mut tape = Tape::default();
    for _ in 0..FAMILY_RING + 1 {
        tape.now += FAMILY_WINDOW_MS;
        for k in keys {
            tape.add(k, BASELINE / keys.len() as u64);
        }
        tape.add(OUT, 200);
        watch.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    }
    tape
}

/// **A member born MID-WINDOW loses no items**, and the `None`-anchor arm is EXACT rather than
/// an approximation.
///
/// `ingest` inserts `Liveness { rows: 0, last_ms: 0 }` on first sight and the counter is
/// per-process, so a member first seen with `rows = N` has produced exactly N items in this
/// window and nothing earlier. Taking `N` in full is therefore right; taking `0` would silently
/// shed a whole birth's worth of a rotation's arrivals, every rotation, forever.
#[test]
fn a_member_born_mid_window_loses_no_items() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A"]);

    // The judged window: A is silent, and NEWBORN arrives mid-window with 7 items already on
    // its counter. Two half-ticks, so the window closes on the second.
    let f = fam(&["A", "NEWBORN"]);
    tape.now += HALF;
    tape.add("NEWBORN", 7);
    tape.add(OUT, 100);
    assert!(w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());

    tape.now += HALF;
    tape.add(OUT, 100);
    let got = w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1, "the window collapsed and must be reported: {got:?}");
    assert_eq!(
        got[0].observed_items, 7,
        "a member born mid-window must contribute every item it has produced"
    );
    assert_eq!(got[0].members, 2, "both members are in the family this window");
}

/// **A member DEPARTING mid-window contributes its tail**, and the family total never goes
/// backwards.
///
/// A token leaves `expected_families` the tick the runtime unsubscribes it — before the rows it
/// produced since the previous tick have been accounted. Collecting a departed key's final
/// delta against its REMEMBERED family, once, on the tick it disappears, is what keeps a
/// rotation's arithmetic exact; dropping it would shed one tick of each dying token's tail
/// (~1 % of a window on the deployed profile) in the direction of a FALSE POSITIVE.
#[test]
fn a_member_departing_mid_window_contributes_its_tail() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A", "LEAVING"]);

    // LEAVING produces 11 more items and is then unsubscribed. A produces nothing.
    tape.now += HALF;
    tape.add("LEAVING", 11);
    tape.add(OUT, 100);
    let f_after = fam(&["A"]);
    assert!(w.family_collapse(&f_after, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());

    tape.now += HALF;
    tape.add(OUT, 100);
    let got = w.family_collapse(&f_after, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].observed_items, 11,
        "a departing member's final delta belongs to the family it was recorded under"
    );
}

/// **THE REGRESSION TEST FOR THE OBVIOUS WRONG DESIGN.** The family total is a SUM OF PER-KEY
/// DELTAS, never a delta of a sum of counters.
///
/// Differencing a summed counter across a CHANGING key set reads zero-or-negative on a HEALTHY
/// rotating family, every window, forever: two departing tokens take their entire lifetime
/// counters out of the sum while two joiners start at zero, and the Polymarket rotation period
/// (300 s) sits inside any window long enough to be worth measuring. `slow_series`' `anchors`
/// idiom is the repair and it was already in this file.
#[test]
fn the_family_total_is_a_sum_of_per_key_deltas_not_a_delta_of_a_sum() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["OLD_UP", "OLD_DOWN"]);
    // The two outgoing members carry large cumulative counters by now.
    let summed_before: u64 = ["OLD_UP", "OLD_DOWN"].iter().map(|k| tape.rows[*k]).sum();
    assert!(summed_before > BASELINE, "the fixture must have real history to lose");

    // The rotation: both old members leave with a last 500 items each, both new ones join at
    // zero and produce 1,500 each. A delta-of-a-sum would read
    // (3,000 + 1,000) - summed_before, i.e. deeply NEGATIVE, and saturate to 0.
    let f_new = fam(&["NEW_UP", "NEW_DOWN"]);
    tape.now += HALF;
    tape.add("OLD_UP", 500);
    tape.add("OLD_DOWN", 500);
    tape.add("NEW_UP", 700);
    tape.add("NEW_DOWN", 700);
    tape.add(OUT, 100);
    assert!(w.family_collapse(&f_new, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());

    tape.now += HALF;
    tape.add("NEW_UP", 800);
    tape.add("NEW_DOWN", 800);
    tape.add(OUT, 100);
    let got = w.family_collapse(&f_new, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    // 500 + 500 (the departing tails) + 1,500 + 1,500 (the newborns) = 4,000, which is 20 % of
    // BASELINE — a healthy rotation dip, NOT a collapse, so nothing fires and the arithmetic is
    // read out of the next window instead.
    assert!(got.is_empty(), "a healthy rotation must not be reported as a collapse: {got:?}");

    // Now a genuinely dark window, whose verdict carries the running total as evidence.
    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    let got = w.family_collapse(&f_new, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].observed_items, 0, "and a dark window is zero, never a wrapped negative");
}

/// A rotation in which the family total holds STEADY is judged steady — the property the whole
/// design rests on, at the mechanism level. Two members leave and two join inside one window,
/// and the total is unchanged, so no verdict is taken and the ring learns the window normally.
#[test]
fn a_rotation_that_holds_the_total_steady_is_invisible() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["OLD_UP", "OLD_DOWN"]);

    for r in 0..6 {
        let old = (format!("R{r}_UP"), format!("R{r}_DOWN"));
        let f = fam(&[&old.0, &old.1]);
        tape.now += FAMILY_WINDOW_MS;
        tape.add(&old.0, BASELINE / 2);
        tape.add(&old.1, BASELINE / 2);
        tape.add(OUT, 200);
        let got = w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert!(got.is_empty(), "rotation {r} was reported as a collapse: {got:?}");
    }
}

/// A window with NO out-of-family witness takes no verdict, even though the family is dark —
/// the whole-process stall, which the recency watchdog owns and which fires first.
#[test]
fn a_window_with_no_out_of_family_activity_takes_no_verdict() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A"]);
    tape.now += FAMILY_WINDOW_MS;
    // Nothing at all arrives — not the family, not the licence.
    let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert!(got.is_empty(), "a stalled process must not be reported as a dead family: {got:?}");
}

/// A family whose baseline is under [`MIN_BASELINE_ITEMS`] is never judged, whatever it does.
/// The resolution gate, at the mechanism level.
#[test]
fn a_baseline_below_the_resolution_gate_is_never_judged() {
    let mut w = SilenceWatch::new();
    let f = fam(&["A"]);
    let mut tape = Tape::default();
    for _ in 0..FAMILY_RING + 4 {
        tape.now += FAMILY_WINDOW_MS;
        tape.add("A", MIN_BASELINE_ITEMS / 40);
        tape.add(OUT, 200);
        assert!(w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());
    }
    for _ in 0..3 {
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let got = w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert!(got.is_empty(), "a thin family must be refused a verdict: {got:?}");
    }
}

/// `expected_families` as it reads on a tick where the family resolved to ZERO symbols — only
/// the out-of-family witness is left. `crates/vike-recorder/src/runtime.rs`'s `tick` documents
/// that state as a real failure ("a family that resolves to zero symbols is the quieter of the
/// two failures this field exists for"), and unlike an `Err` it does not `continue`.
fn only_out() -> Vec<(String, String)> {
    vec![(OUT.to_string(), OUT.to_string())]
}

/// **A family that blinks out of the profile for ONE tick keeps its baseline.**
///
/// The state used to die on the first tick a family was absent, so an `Ok(empty)` market-list
/// response — one tick — discarded a full ring and the family was UNJUDGED for the next
/// [`FAMILY_RING`] windows, silently. Ten minutes blind, with nothing in the log to say so, on
/// a failure adjacent to the very incident class this rule targets.
#[test]
fn a_family_absent_for_one_tick_keeps_its_baseline() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A"]);

    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    assert!(
        w.family_collapse(&only_out(), &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty(),
        "a family with no members this window has no honest total, so it takes no verdict"
    );

    // …and it is back on the very next tick, dark.
    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1, "the blink must not have discarded the ring: {got:?}");
    assert_eq!(got[0].ring_windows, FAMILY_RING, "…and the ring is still the one it learned");
    assert_eq!(got[0].baseline_items, BASELINE, "…against the baseline it already had");
}

/// **A member that blinks out contributes its DELTA on return, never its whole lifetime.**
///
/// The departed pass used to forget a member's anchor outright, so a member absent for one tick
/// hit the first-sight arm on its return and booked its entire `Liveness::rows` counter — a
/// Polymarket token's whole ~600 s life — into that one window, which then TAUGHT the ring.
/// Fixed beside the grace above rather than after it: keeping a ring alive across a blink while
/// letting the blink poison it would be half a repair.
#[test]
fn a_member_that_blinks_out_contributes_its_delta_not_its_lifetime_on_return() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A"]);
    let lifetime = tape.rows["A"];
    assert!(lifetime > BASELINE * 10, "the fixture must have a lifetime worth over-counting");

    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    let _ = w.family_collapse(&only_out(), &tape.live(), tape.now, FAMILY_WINDOW_MS);

    // A is back and has produced NOTHING since. With its anchor kept, that window is zero and
    // fires; with the anchor forgotten it would read `lifetime` and be the busiest window the
    // family ever had.
    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1, "a dark window after a blink is still a dark window: {got:?}");
    assert_eq!(got[0].observed_items, 0, "a returning member is re-anchored, not re-counted");
}

/// **…and an absence longer than the ring's own span DOES discard it** — the grace is one ring
/// length precisely because every entry in a ring older than that describes a window the ring
/// no longer claims to cover. See [`FAMILY_ABSENCE_GRACE_MS`].
///
/// ⚠ **The returning member is a NEW token, and that is what makes this test mean anything.**
/// A first draft brought the SAME member back, and it passed whether or not the expiry fired:
/// by then that member's anchor had been pruned on the same clock, so its return booked its
/// whole lifetime, the window read as enormous rather than dark, and no verdict was taken for a
/// reason that had nothing to do with the ring. Mutating the expiry to `i64::MAX` left it green
/// — a vacuous test, caught only by mutating the thing it claimed to pin. A fresh token is also
/// the realistic fixture: ten minutes is two Polymarket rotation periods, so the family that
/// comes back does not hold the members that left.
#[test]
fn a_family_absent_past_the_grace_relearns_from_scratch() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A"]);

    for _ in 0..FAMILY_ABSENCE_GRACE_MS / FAMILY_WINDOW_MS + 1 {
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let _ = w.family_collapse(&only_out(), &tape.live(), tape.now, FAMILY_WINDOW_MS);
    }

    // The family is back, with a token born during the absence, and dark. A kept ring would
    // judge that against the regime of ten minutes ago and fire.
    let back = fam(&["B"]);
    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    let got = w.family_collapse(&back, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert!(
        got.is_empty(),
        "a ring older than the span it claims to describe must be discarded, not re-used: \
             {got:?}"
    );

    // …and it RELEARNS rather than being permanently dead: a full ring at the new regime, then
    // a dark window, and it judges again — against the baseline it just learned.
    for _ in 0..FAMILY_RING {
        tape.now += FAMILY_WINDOW_MS;
        tape.add("B", BASELINE / 2);
        tape.add(OUT, 200);
        let _ = w.family_collapse(&back, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    }
    tape.now += FAMILY_WINDOW_MS;
    tape.add(OUT, 200);
    let got = w.family_collapse(&back, &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1, "a relearned family must be judged again: {got:?}");
    assert_eq!(
        got[0].baseline_items,
        BASELINE / 2,
        "…against the regime it learned on its return, not the one it left"
    );
}

/// **A witness reduced to disconnect MARKERS cannot licence a verdict** — the whole-process
/// stall this rule declares it withholds on, at the mechanism level.
///
/// The licence was `licence_items > 0` over raw `Liveness::rows`, which counts the
/// `GapStart`/`Stale`/`LiveResume` rows `crates/vike-data/src/live_rec.rs`'s `stream_status`
/// writes — rows emitted BECAUSE data stopped. One marker from one other lane therefore
/// licensed a verdict during a host-wide network loss, and the alert body asserted "this
/// recorder was still receiving data" on the strength of a disconnect.
#[test]
fn a_witness_reduced_to_markers_cannot_licence_a_verdict() {
    let mut w = SilenceWatch::new();
    let mut tape = primed(&mut w, &["A"]);

    // The host loses the network: the family falls to its own handful of markers and the
    // witness to a couple of its own. Non-zero on both sides, which is the case the old
    // `> 0` predicate could not tell from health.
    tape.now += FAMILY_WINDOW_MS;
    tape.add("A", 1);
    tape.add(OUT, 2);
    let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert!(got.is_empty(), "a stalled process must not vouch for itself: {got:?}");

    // …and the SAME dark family fires the moment the witness is genuinely writing again, so
    // this is a licence test rather than a rule that stopped working.
    tape.now += FAMILY_WINDOW_MS;
    tape.add("A", 1);
    tape.add(OUT, 200);
    let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
    assert_eq!(got.len(), 1, "a live witness licenses the same window: {got:?}");
    assert_eq!(got[0].licence, OUT);
}

/// A non-positive window is inert, matching [`SilenceWatch::slow_series`]' own guard.
#[test]
fn a_non_positive_window_judges_nothing() {
    let mut w = SilenceWatch::new();
    let mut tape = Tape { now: 1_000, ..Default::default() };
    tape.add("A", 5);
    assert!(w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, 0).is_empty());
}
