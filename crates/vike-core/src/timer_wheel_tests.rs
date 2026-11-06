use super::*;

/// A wheel with millisecond resolution (tick_ms = 1) so deadlines map 1:1 to ms — the clearest
/// setting for the ordering/cancellation semantics.
fn wheel() -> DeadlineTimerWheel<u32> {
    DeadlineTimerWheel::new(1, 64)
}

#[test]
fn empty_advance_is_noop() {
    let mut w = wheel();
    let mut out = Vec::new();
    w.advance(1_000, &mut out);
    assert!(out.is_empty());
    assert!(w.is_empty());
}

#[test]
fn fires_at_or_after_deadline_not_before() {
    let mut w = wheel();
    w.advance(100, &mut Vec::new()); // pin cursor at 100
    w.insert(150, 7);
    let mut out = Vec::new();
    w.advance(149, &mut out);
    assert!(out.is_empty(), "must not fire before the deadline");
    w.advance(150, &mut out);
    assert_eq!(out, vec![7], "fires exactly at the deadline");
    assert!(w.is_empty());
}

#[test]
fn expires_in_nondecreasing_deadline_order() {
    let mut w = wheel();
    w.advance(0, &mut Vec::new());
    // insert out of deadline order
    w.insert(300, 3);
    w.insert(100, 1);
    w.insert(200, 2);
    w.insert(250, 25);
    let mut out = Vec::new();
    w.advance(1_000, &mut out);
    assert_eq!(out, vec![1, 2, 25, 3], "emitted in ascending deadline order");
}

#[test]
fn partial_advance_only_expires_due_timers() {
    let mut w = wheel();
    w.advance(0, &mut Vec::new());
    w.insert(100, 1);
    w.insert(200, 2);
    w.insert(300, 3);
    let mut out = Vec::new();
    w.advance(200, &mut out);
    assert_eq!(out, vec![1, 2]);
    assert_eq!(w.len(), 1, "the 300 timer is still armed");
    out.clear();
    w.advance(300, &mut out);
    assert_eq!(out, vec![3]);
    assert!(w.is_empty());
}

#[test]
fn cancel_removes_before_firing_and_returns_payload() {
    let mut w = wheel();
    w.advance(0, &mut Vec::new());
    let id = w.insert(500, 42);
    w.insert(600, 43);
    assert_eq!(w.cancel(id), Some(42));
    assert_eq!(w.len(), 1);
    let mut out = Vec::new();
    w.advance(1_000, &mut out);
    assert_eq!(out, vec![43], "the cancelled timer never fires");
}

#[test]
fn cancel_is_idempotent_and_generation_safe() {
    let mut w = wheel();
    w.advance(0, &mut Vec::new());
    let id = w.insert(500, 1);
    assert_eq!(w.cancel(id), Some(1));
    assert_eq!(w.cancel(id), None, "double-cancel is a no-op");
    // reuse the freed slab slot; the stale id must not match the new timer.
    let id2 = w.insert(700, 2);
    assert_eq!(id2.idx, id.idx, "slab slot reused");
    assert_ne!(id2, id, "generation differs");
    assert_eq!(w.cancel(id), None, "the stale id cannot cancel the reused slot");
    assert_eq!(w.len(), 1);
    assert_eq!(w.cancel(id2), Some(2));
}

#[test]
fn cancelling_the_minimum_still_fires_the_rest() {
    // exercises the "cancel leaves min_tick a loose lower bound" self-healing path.
    let mut w = wheel();
    w.advance(0, &mut Vec::new());
    let early = w.insert(100, 1);
    w.insert(200, 2);
    assert_eq!(w.cancel(early), Some(1));
    let mut out = Vec::new();
    w.advance(150, &mut out);
    assert!(out.is_empty(), "the only <=150 timer was cancelled");
    w.advance(250, &mut out);
    assert_eq!(out, vec![2]);
}

#[test]
fn past_deadline_fires_on_next_advance() {
    let mut w = wheel();
    w.advance(1_000, &mut Vec::new()); // cursor at 1000
    w.insert(500, 9); // already in the past
    let mut out = Vec::new();
    w.advance(1_001, &mut out);
    assert_eq!(out, vec![9], "a past deadline fires promptly, not a rotation later");
}

#[test]
fn deadlines_beyond_one_rotation_still_fire() {
    // num_slots = 8 ⇒ rotation = 8 ticks; schedule well beyond it and jump across in one advance
    // (the full-rotation expiry path).
    let mut w: DeadlineTimerWheel<u32> = DeadlineTimerWheel::new(1, 8);
    w.advance(0, &mut Vec::new());
    w.insert(5, 5);
    w.insert(37, 37); // > 4 rotations away, same slot as tick 5 (5 % 8 == 37 % 8)
    w.insert(64, 64);
    let mut out = Vec::new();
    w.advance(100, &mut out);
    assert_eq!(out, vec![5, 37, 64], "all fire, in order, across many rotations");
    assert!(w.is_empty());
}

#[test]
fn stepping_and_full_pass_agree_across_a_rotation_boundary() {
    // Same schedule advanced two ways: one tick at a time (stepping) vs one big jump
    // (full-rotation pass). Both must emit the identical ordered batch.
    let schedule = [(3u32, 3i64), (9, 9), (12, 12), (20, 20), (5, 5)];
    let mut stepped = Vec::new();
    {
        let mut w: DeadlineTimerWheel<u32> = DeadlineTimerWheel::new(1, 8);
        w.advance(0, &mut Vec::new());
        for (item, dl) in schedule {
            w.insert(dl, item);
        }
        for now in 1..=25 {
            w.advance(now, &mut stepped);
        }
    }
    let mut jumped = Vec::new();
    {
        let mut w: DeadlineTimerWheel<u32> = DeadlineTimerWheel::new(1, 8);
        w.advance(0, &mut Vec::new());
        for (item, dl) in schedule {
            w.insert(dl, item);
        }
        w.advance(25, &mut jumped);
    }
    assert_eq!(stepped, vec![3, 5, 9, 12, 20]);
    assert_eq!(jumped, stepped, "stepping and one-shot advance agree");
}

#[test]
fn advance_across_many_future_deadlines_is_selective() {
    // "O(1) advance across many deadlines": with 10k timers far in the future plus one near,
    // advancing just past the near one expires EXACTLY that one — the 10k others are neither
    // scanned into `out` nor removed. The pre-`min_tick` fast path also skips cheaply.
    let mut w = DeadlineTimerWheel::new(1, 1024);
    w.advance(0, &mut Vec::new());
    for i in 0..10_000u32 {
        w.insert(1_000_000 + i as i64, i);
    }
    w.insert(500, 7);
    let mut out = Vec::new();
    // many advances that cross NO deadline: fast-skip, nothing expires.
    for now in [10, 50, 100, 400] {
        w.advance(now, &mut out);
        assert!(out.is_empty());
    }
    w.advance(500, &mut out);
    assert_eq!(out, vec![7], "only the due timer expires");
    assert_eq!(w.len(), 10_000, "the far-future timers are untouched");
}

#[test]
fn no_allocation_on_the_steady_path() {
    // Warm the slab to a steady concurrent peak, then run many insert→expire cycles and assert
    // the slab never grows again (nodes are reused via the free list) and the reused output
    // buffer never regrows.
    let mut w = DeadlineTimerWheel::new(1, 256);
    w.advance(0, &mut Vec::new());
    let mut out = Vec::new();
    let mut now = 0i64;
    // warm up: keep ~64 timers concurrently in flight for a few rounds.
    for round in 0..8 {
        for k in 0..64 {
            w.insert(now + 10 + k, round * 64 + k as u32);
        }
        now += 100;
        w.advance(now, &mut out);
        out.clear();
    }
    let cap_after_warmup = w.node_capacity();
    let out_cap_after_warmup = out.capacity();
    assert!(cap_after_warmup >= 64);
    for round in 8..2_000 {
        for k in 0..64 {
            w.insert(now + 10 + k, round * 64 + k as u32);
        }
        now += 100;
        w.advance(now, &mut out);
        out.clear();
    }
    assert_eq!(w.node_capacity(), cap_after_warmup, "slab did not reallocate on the steady path");
    assert_eq!(out.capacity(), out_cap_after_warmup, "reused output buffer did not regrow");
    assert!(w.is_empty());
}

#[test]
fn deterministic_under_injected_now() {
    // The wheel reads no clock: the same inserts + the same now_ms sequence give byte-identical
    // expiries every run.
    let run = || {
        let mut w = DeadlineTimerWheel::new(5, 128); // coarse resolution to exercise quantization
        w.advance(1_000, &mut Vec::new());
        w.insert(1_007, 1);
        w.insert(1_050, 2);
        w.insert(1_023, 3);
        let mut trace = Vec::new();
        for now in (1_000..=1_100).step_by(5) {
            let mut out = Vec::new();
            w.advance(now, &mut out);
            for item in out {
                trace.push((now, item));
            }
        }
        trace
    };
    assert_eq!(run(), run(), "identical now_ms sequence ⇒ identical expiries");
    // and the coarse-resolution timers never fire before their real deadline
    let trace = run();
    for (fired_at, item) in trace {
        let deadline = match item {
            1 => 1_007,
            2 => 1_050,
            3 => 1_023,
            _ => unreachable!(),
        };
        assert!(fired_at >= deadline, "item {item} fired at {fired_at} before {deadline}");
    }
}

#[test]
fn monotonic_advance_ignores_backward_now() {
    let mut w = wheel();
    w.advance(1_000, &mut Vec::new());
    w.insert(1_100, 1);
    let mut out = Vec::new();
    w.advance(900, &mut out); // backward — ignored
    assert!(out.is_empty());
    w.advance(1_100, &mut out);
    assert_eq!(out, vec![1]);
}
